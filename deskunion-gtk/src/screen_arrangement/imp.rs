use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use adw::subclass::prelude::*;
use gtk::glib::clone;
use gtk::glib::subclass::Signal;
use gtk::graphene::{Point, Rect};
use gtk::{gdk, glib, graphene, gsk, prelude::*};

use deskunion_ipc::Position;

use super::ScreenItem;

const HOST_W: f64 = 92.0;
const HOST_H: f64 = 60.0;
const SAT_W: f64 = 80.0;
const SAT_H: f64 = 52.0;
const GAP: f64 = 32.0;
/// perpendicular offset applied to the 2nd, 3rd, ... screen stacked at
/// the same border (only possible for *configured*, not *active*,
/// clients — the protocol allows one active client per border)
const STACK_OFFSET: f64 = 12.0;
const CORNER_RADIUS: f32 = 10.0;
const BADGE_RADIUS: f64 = 5.0;

#[derive(Default)]
pub struct ScreenArrangement {
    items: RefCell<Vec<ScreenItem>>,
    /// index into `items` of the screen currently being dragged
    dragging: Cell<Option<usize>>,
    drag_start: Cell<(f64, f64)>,
    drag_offset: Cell<(f64, f64)>,
}

#[glib::object_subclass]
impl ObjectSubclass for ScreenArrangement {
    const NAME: &'static str = "ScreenArrangement";
    type Type = super::ScreenArrangement;
    type ParentType = gtk::Widget;
}

impl ObjectImpl for ScreenArrangement {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.set_hexpand(true);
        obj.set_vexpand(true);

        let gesture = gtk::GestureDrag::new();
        gesture.connect_drag_begin(clone!(
            #[weak(rename_to = widget)]
            self,
            move |_gesture, x, y| widget.on_drag_begin(x, y)
        ));
        gesture.connect_drag_update(clone!(
            #[weak(rename_to = widget)]
            self,
            move |_gesture, x, y| widget.on_drag_update(x, y)
        ));
        gesture.connect_drag_end(clone!(
            #[weak(rename_to = widget)]
            self,
            move |_gesture, x, y| widget.on_drag_end(x, y)
        ));
        obj.add_controller(gesture);
    }

    fn signals() -> &'static [Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![
                // (ClientHandle of the repositioned screen, new position
                // as lowercase text — "left"/"right"/"top"/"bottom")
                Signal::builder("position-changed")
                    .param_types([u64::static_type(), String::static_type()])
                    .build(),
            ]
        })
    }
}

impl WidgetImpl for ScreenArrangement {
    fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
        let size = match orientation {
            gtk::Orientation::Horizontal => 280,
            _ => 200,
        };
        (size, size, -1, -1)
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let obj = self.obj();
        let width = obj.width() as f64;
        let height = obj.height() as f64;
        if width <= 0.0 || height <= 0.0 {
            return;
        }

        let fg = obj.color();
        // `StyleContext` is deprecated since GTK 4.10, but named-color
        // lookup (libadwaita's `@define-color accent_bg_color` etc.) has
        // no non-deprecated replacement in gtk4-rs yet — the underlying
        // C function is still fully supported, just flagged.
        #[allow(deprecated)]
        let style = obj.style_context();
        #[allow(deprecated)]
        let accent = style
            .lookup_color("accent_bg_color")
            .unwrap_or(gdk::RGBA::new(0.3, 0.5, 0.9, 1.0));
        #[allow(deprecated)]
        let success = style
            .lookup_color("success_color")
            .unwrap_or(gdk::RGBA::new(0.2, 0.7, 0.3, 1.0));
        let muted = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12);
        let dim = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.35);

        let host_rect = Rect::new(
            ((width - HOST_W) / 2.0) as f32,
            ((height - HOST_H) / 2.0) as f32,
            HOST_W as f32,
            HOST_H as f32,
        );
        let widget: &gtk::Widget = obj.upcast_ref();
        draw_screen(
            widget,
            snapshot,
            &host_rect,
            &accent,
            &fg,
            "This device",
            false,
        );

        let dragging = self.dragging.get();
        let items = self.items.borrow();
        for (i, position, rect) in self.layout_rects(width, height) {
            let rect = if dragging == Some(i) {
                let (ox, oy) = self.drag_offset.get();
                Rect::new(
                    rect.x() + ox as f32,
                    rect.y() + oy as f32,
                    rect.width(),
                    rect.height(),
                )
            } else {
                rect
            };

            let Some(item) = items.get(i) else { continue };
            let label = item
                .hostname
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("client @ {position}"));

            let color = if item.active { &success } else { &muted };
            draw_screen(
                widget,
                snapshot,
                &rect,
                color,
                if item.active { &fg } else { &dim },
                &label,
                item.audio_active,
            );
        }
    }
}

fn draw_screen(
    widget: &gtk::Widget,
    snapshot: &gtk::Snapshot,
    rect: &Rect,
    fill: &gdk::RGBA,
    text_color: &gdk::RGBA,
    label: &str,
    audio_badge: bool,
) {
    let rounded = gsk::RoundedRect::from_rect(*rect, CORNER_RADIUS);
    snapshot.push_rounded_clip(&rounded);
    snapshot.append_color(fill, rect);
    snapshot.pop();

    let border_color = [*text_color, *text_color, *text_color, *text_color];
    snapshot.append_border(&rounded, &[1.5, 1.5, 1.5, 1.5], &border_color);

    let layout = widget.create_pango_layout(Some(label));
    let inner_width = (rect.width() - 10.0).max(1.0);
    layout.set_width((inner_width * gtk::pango::SCALE as f32) as i32);
    layout.set_alignment(gtk::pango::Alignment::Center);
    layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let (_, text_h) = layout.pixel_size();
    let text_y = rect.y() + (rect.height() - text_h as f32) / 2.0;
    snapshot.save();
    snapshot.translate(&Point::new(rect.x() + 5.0, text_y));
    snapshot.append_layout(&layout, text_color);
    snapshot.restore();

    if audio_badge {
        let badge_center = Point::new(
            rect.x() + rect.width() - BADGE_RADIUS as f32 - 4.0,
            rect.y() + BADGE_RADIUS as f32 + 4.0,
        );
        let badge_rect = Rect::new(
            badge_center.x() - BADGE_RADIUS as f32,
            badge_center.y() - BADGE_RADIUS as f32,
            BADGE_RADIUS as f32 * 2.0,
            BADGE_RADIUS as f32 * 2.0,
        );
        let badge = gsk::RoundedRect::from_rect(badge_rect, BADGE_RADIUS as f32);
        snapshot.push_rounded_clip(&badge);
        snapshot.append_color(&gdk::RGBA::new(0.95, 0.65, 0.15, 1.0), &badge_rect);
        snapshot.pop();
    }
}

impl ScreenArrangement {
    pub(super) fn set_items(&self, items: Vec<ScreenItem>) {
        self.items.replace(items);
        self.obj().queue_draw();
    }

    /// (index, position, rect) for every screen currently laid out —
    /// shared between `snapshot()` and the drag hit-testing/geometry.
    fn layout_rects(&self, width: f64, height: f64) -> Vec<(usize, Position, Rect)> {
        let items = self.items.borrow();
        let host_cx = width / 2.0;
        let host_cy = height / 2.0;

        // count how many configured clients share each border, to
        // stack them with a small perpendicular offset instead of
        // drawing them fully on top of each other
        let mut seen_per_position: std::collections::HashMap<Position, usize> = Default::default();

        let mut out = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let position = item.position;
            let stack_index = *seen_per_position
                .entry(position)
                .and_modify(|n| *n += 1)
                .or_insert(0);
            let stack = stack_index as f64 * STACK_OFFSET;

            let (x, y) = match position {
                Position::Left => (
                    host_cx - HOST_W / 2.0 - GAP - SAT_W - stack,
                    host_cy - SAT_H / 2.0,
                ),
                Position::Right => (host_cx + HOST_W / 2.0 + GAP + stack, host_cy - SAT_H / 2.0),
                Position::Top => (
                    host_cx - SAT_W / 2.0,
                    host_cy - HOST_H / 2.0 - GAP - SAT_H - stack,
                ),
                Position::Bottom => (host_cx - SAT_W / 2.0, host_cy + HOST_H / 2.0 + GAP + stack),
            };
            out.push((
                i,
                position,
                Rect::new(x as f32, y as f32, SAT_W as f32, SAT_H as f32),
            ));
        }
        out
    }

    fn on_drag_begin(&self, x: f64, y: f64) {
        let obj = self.obj();
        let width = obj.width() as f64;
        let height = obj.height() as f64;
        let hit = self
            .layout_rects(width, height)
            .into_iter()
            .find(|(_, _, rect)| {
                let p = graphene::Point::new(x as f32, y as f32);
                rect.contains_point(&p)
            })
            .map(|(i, ..)| i);
        self.dragging.set(hit);
        self.drag_start.set((x, y));
        self.drag_offset.set((0.0, 0.0));
    }

    fn on_drag_update(&self, offset_x: f64, offset_y: f64) {
        if self.dragging.get().is_none() {
            return;
        }
        self.drag_offset.set((offset_x, offset_y));
        self.obj().queue_draw();
    }

    fn on_drag_end(&self, offset_x: f64, offset_y: f64) {
        let Some(index) = self.dragging.take() else {
            return;
        };
        self.drag_offset.set((0.0, 0.0));
        let obj = self.obj();
        let width = obj.width() as f64;
        let height = obj.height() as f64;
        let (start_x, start_y) = self.drag_start.get();
        let end_x = start_x + offset_x;
        let end_y = start_y + offset_y;

        // snap to the nearest border of the host box, not a free
        // coordinate — the protocol only has left/right/top/bottom
        // (see `deskunion_proto::Position`)
        let dx = end_x - width / 2.0;
        let dy = end_y - height / 2.0;
        let position = if dx.abs() > dy.abs() {
            if dx > 0.0 {
                Position::Right
            } else {
                Position::Left
            }
        } else if dy > 0.0 {
            Position::Bottom
        } else {
            Position::Top
        };

        let Some(item) = self.items.borrow().get(index).cloned() else {
            return;
        };

        obj.queue_draw();
        obj.emit_by_name::<()>("position-changed", &[&item.handle, &position.to_string()]);
    }
}
