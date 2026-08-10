use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use adw::subclass::prelude::*;
use gtk::glib::clone;
use gtk::glib::subclass::Signal;
use gtk::graphene::{Point, Rect};
use gtk::{gdk, glib, gsk, prelude::*};

use deskunion_ipc::Position;

use super::ScreenItem;

const HOST_W: f64 = 116.0;
const HOST_H: f64 = 74.0;
const SAT_W: f64 = 100.0;
const SAT_H: f64 = 64.0;
const GAP: f64 = 32.0;
const CANVAS_PADDING: f64 = 16.0;
const MIN_DRAG_DISTANCE: f64 = 6.0;
/// perpendicular offset applied to the 2nd, 3rd, ... screen stacked at
/// the same border (only possible for *configured*, not *active*,
/// clients — the protocol allows one active client per border)
const STACK_OFFSET: f64 = 12.0;
const CORNER_RADIUS: f32 = 10.0;
const BADGE_RADIUS: f64 = 5.0;

struct Layout {
    host: Rect,
    screens: Vec<(usize, Position, Rect)>,
    scale: f64,
}

struct ScreenStyle<'a> {
    fill: &'a gdk::RGBA,
    text: &'a gdk::RGBA,
    success: &'a gdk::RGBA,
    selection: &'a gdk::RGBA,
    audio_active: bool,
    selected: bool,
    hovered: bool,
    active: bool,
}

#[derive(Default)]
pub struct ScreenArrangement {
    items: RefCell<Vec<ScreenItem>>,
    /// label for the local (blue) screen; empty falls back to "This device"
    host_label: RefCell<String>,
    /// index into `items` of the screen currently being dragged
    dragging: Cell<Option<usize>>,
    drag_start: Cell<(f64, f64)>,
    drag_offset: Cell<(f64, f64)>,
    drop_position: Cell<Option<Position>>,
    selected: Cell<Option<u64>>,
    hovered: Cell<Option<usize>>,
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
        obj.set_focusable(true);
        obj.set_tooltip_text(Some(
            "Drag a device to choose the screen edge used to reach it",
        ));

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

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(clone!(
            #[weak(rename_to = widget)]
            self,
            move |_motion, x, y| widget.on_motion(x, y)
        ));
        motion.connect_leave(clone!(
            #[weak(rename_to = widget)]
            self,
            move |_motion| widget.on_leave()
        ));
        obj.add_controller(motion);
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
        let accent_fg = style
            .lookup_color("accent_fg_color")
            .unwrap_or(gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
        #[allow(deprecated)]
        let success = style
            .lookup_color("success_color")
            .unwrap_or(gdk::RGBA::new(0.2, 0.7, 0.3, 1.0));
        let muted = with_alpha(&fg, 0.08);
        let active = with_alpha(&success, 0.16);
        let dim = with_alpha(&fg, 0.55);
        let layout = self.layout(width, height);
        let widget: &gtk::Widget = obj.upcast_ref();

        draw_grid(snapshot, width, height, &with_alpha(&fg, 0.16));

        if let Some(target) = self.drop_position.get() {
            for position in [
                Position::Left,
                Position::Right,
                Position::Top,
                Position::Bottom,
            ] {
                draw_drop_zone(
                    widget,
                    snapshot,
                    &drop_zone_rect(&layout, position),
                    &accent,
                    position == target,
                    &position.to_string(),
                );
            }
        }

        let host_label = self.host_label.borrow();
        draw_screen(
            widget,
            snapshot,
            &layout.host,
            if host_label.is_empty() {
                "This device"
            } else {
                &host_label
            },
            &ScreenStyle {
                fill: &accent,
                text: &accent_fg,
                success: &success,
                selection: &accent,
                audio_active: false,
                selected: false,
                hovered: false,
                active: false,
            },
        );

        let dragging = self.dragging.get();
        let hovered = self.hovered.get();
        let selected = self.selected.get();
        let items = self.items.borrow();
        let mut screens = layout.screens;
        screens.sort_by_key(|(index, ..)| {
            dragging == Some(*index)
                || items
                    .get(*index)
                    .is_some_and(|item| selected == Some(item.handle))
        });
        for (i, position, rect) in screens {
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

            let color = if item.active { &active } else { &muted };
            draw_screen(
                widget,
                snapshot,
                &rect,
                &label,
                &ScreenStyle {
                    fill: color,
                    text: if item.active { &fg } else { &dim },
                    success: &success,
                    selection: &accent,
                    audio_active: item.audio_active,
                    selected: selected == Some(item.handle),
                    hovered: hovered == Some(i),
                    active: item.active,
                },
            );
        }

        if items.is_empty() {
            draw_hint(
                widget,
                snapshot,
                width,
                layout.host.y() as f64 + layout.host.height() as f64 + 18.0,
                &dim,
                "Add a client to arrange it here",
            );
        }
    }
}

fn with_alpha(color: &gdk::RGBA, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(color.red(), color.green(), color.blue(), alpha)
}

fn draw_grid(snapshot: &gtk::Snapshot, width: f64, height: f64, color: &gdk::RGBA) {
    const STEP: f64 = 24.0;
    const DOT: f32 = 1.5;
    let mut y = STEP / 2.0;
    while y < height {
        let mut x = STEP / 2.0;
        while x < width {
            snapshot.append_color(color, &Rect::new(x as f32, y as f32, DOT, DOT));
            x += STEP;
        }
        y += STEP;
    }
}

fn draw_screen(
    widget: &gtk::Widget,
    snapshot: &gtk::Snapshot,
    rect: &Rect,
    label: &str,
    style: &ScreenStyle<'_>,
) {
    let rounded = gsk::RoundedRect::from_rect(*rect, CORNER_RADIUS);
    snapshot.push_rounded_clip(&rounded);
    snapshot.append_color(style.fill, rect);
    snapshot.pop();

    let border_width = if style.selected {
        3.0
    } else if style.hovered {
        2.0
    } else {
        1.0
    };
    let outline = if style.selected {
        *style.selection
    } else {
        *style.text
    };
    let border_color = [outline; 4];
    snapshot.append_border(&rounded, &[border_width; 4], &border_color);

    let layout = widget.create_pango_layout(Some(label));
    let inner_width = (rect.width() - 10.0).max(1.0);
    layout.set_width((inner_width * gtk::pango::SCALE as f32) as i32);
    layout.set_alignment(gtk::pango::Alignment::Center);
    layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let (_, text_h) = layout.pixel_size();
    let text_y = rect.y() + (rect.height() - text_h as f32) / 2.0;
    snapshot.save();
    snapshot.translate(&Point::new(rect.x() + 5.0, text_y));
    snapshot.append_layout(&layout, style.text);
    snapshot.restore();

    if style.active {
        draw_badge(
            snapshot,
            Point::new(
                rect.x() + BADGE_RADIUS as f32 + 5.0,
                rect.y() + rect.height() - BADGE_RADIUS as f32 - 5.0,
            ),
            style.success,
        );
    }

    if style.audio_active {
        draw_badge(
            snapshot,
            Point::new(
                rect.x() + rect.width() - BADGE_RADIUS as f32 - 5.0,
                rect.y() + BADGE_RADIUS as f32 + 5.0,
            ),
            &gdk::RGBA::new(0.95, 0.65, 0.15, 1.0),
        );
    }
}

fn draw_badge(snapshot: &gtk::Snapshot, center: Point, color: &gdk::RGBA) {
    let rect = Rect::new(
        center.x() - BADGE_RADIUS as f32,
        center.y() - BADGE_RADIUS as f32,
        BADGE_RADIUS as f32 * 2.0,
        BADGE_RADIUS as f32 * 2.0,
    );
    let badge = gsk::RoundedRect::from_rect(rect, BADGE_RADIUS as f32);
    snapshot.push_rounded_clip(&badge);
    snapshot.append_color(color, &rect);
    snapshot.pop();
}

fn draw_drop_zone(
    widget: &gtk::Widget,
    snapshot: &gtk::Snapshot,
    rect: &Rect,
    accent: &gdk::RGBA,
    highlighted: bool,
    label: &str,
) {
    let rounded = gsk::RoundedRect::from_rect(*rect, CORNER_RADIUS);
    let color = with_alpha(accent, if highlighted { 0.28 } else { 0.07 });
    snapshot.push_rounded_clip(&rounded);
    snapshot.append_color(&color, rect);
    snapshot.pop();
    snapshot.append_border(
        &rounded,
        &[if highlighted { 2.0 } else { 1.0 }; 4],
        &[*accent; 4],
    );

    let layout = widget.create_pango_layout(Some(label));
    layout.set_alignment(gtk::pango::Alignment::Center);
    let (text_w, text_h) = layout.pixel_size();
    snapshot.save();
    snapshot.translate(&Point::new(
        rect.x() + (rect.width() - text_w as f32) / 2.0,
        rect.y() + (rect.height() - text_h as f32) / 2.0,
    ));
    snapshot.append_layout(&layout, accent);
    snapshot.restore();
}

fn draw_hint(
    widget: &gtk::Widget,
    snapshot: &gtk::Snapshot,
    width: f64,
    y: f64,
    color: &gdk::RGBA,
    text: &str,
) {
    let layout = widget.create_pango_layout(Some(text));
    layout.set_alignment(gtk::pango::Alignment::Center);
    let (text_w, _) = layout.pixel_size();
    snapshot.save();
    snapshot.translate(&Point::new(
        ((width - text_w as f64) / 2.0) as f32,
        y as f32,
    ));
    snapshot.append_layout(&layout, color);
    snapshot.restore();
}

impl ScreenArrangement {
    pub(super) fn set_host_label(&self, label: &str) {
        if *self.host_label.borrow() != label {
            self.host_label.replace(label.to_string());
            self.obj().queue_draw();
        }
    }

    pub(super) fn set_items(&self, items: Vec<ScreenItem>) {
        if self
            .selected
            .get()
            .is_some_and(|handle| !items.iter().any(|item| item.handle == handle))
        {
            self.selected.set(None);
        }
        self.items.replace(items);
        self.obj().queue_draw();
    }

    /// (index, position, rect) for every screen currently laid out —
    /// shared between `snapshot()` and the drag hit-testing/geometry.
    fn layout(&self, width: f64, height: f64) -> Layout {
        let items = self.items.borrow();
        let mut totals: std::collections::HashMap<Position, usize> = Default::default();
        for item in items.iter() {
            *totals.entry(item.position).or_default() += 1;
        }
        let mut seen_per_position: std::collections::HashMap<Position, usize> = Default::default();
        let mut base_rects = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let position = item.position;
            let seen = seen_per_position.entry(position).or_default();
            let total = totals[&position];
            let stack = (*seen as f64 - (total.saturating_sub(1)) as f64 / 2.0) * STACK_OFFSET;
            *seen += 1;

            let (x, y) = match position {
                Position::Left => (-HOST_W / 2.0 - GAP - SAT_W, -SAT_H / 2.0 + stack),
                Position::Right => (HOST_W / 2.0 + GAP, -SAT_H / 2.0 + stack),
                Position::Top => (-SAT_W / 2.0 + stack, -HOST_H / 2.0 - GAP - SAT_H),
                Position::Bottom => (-SAT_W / 2.0 + stack, HOST_H / 2.0 + GAP),
            };
            base_rects.push((
                i,
                position,
                Rect::new(x as f32, y as f32, SAT_W as f32, SAT_H as f32),
            ));
        }

        // Keep the transform stable while devices move, like GNOME's
        // display arrangement. All four possible drop zones are part of
        // the bounds even when no client currently occupies an edge.
        let mut min_x = -HOST_W / 2.0 - GAP - SAT_W;
        let mut max_x = HOST_W / 2.0 + GAP + SAT_W;
        let mut min_y = -HOST_H / 2.0 - GAP - SAT_H;
        let mut max_y = HOST_H / 2.0 + GAP + SAT_H;
        for (_, _, rect) in &base_rects {
            min_x = min_x.min(rect.x() as f64);
            max_x = max_x.max((rect.x() + rect.width()) as f64);
            min_y = min_y.min(rect.y() as f64);
            max_y = max_y.max((rect.y() + rect.height()) as f64);
        }

        let available_w = (width - CANVAS_PADDING * 2.0).max(1.0);
        let available_h = (height - CANVAS_PADDING * 2.0).max(1.0);
        let scale = (available_w / (max_x - min_x))
            .min(available_h / (max_y - min_y))
            .min(1.0);
        let content_cx = (min_x + max_x) / 2.0;
        let content_cy = (min_y + max_y) / 2.0;
        let translate_x = width / 2.0 - content_cx * scale;
        let translate_y = height / 2.0 - content_cy * scale;

        let transform = |rect: &Rect| {
            Rect::new(
                (rect.x() as f64 * scale + translate_x) as f32,
                (rect.y() as f64 * scale + translate_y) as f32,
                (rect.width() as f64 * scale) as f32,
                (rect.height() as f64 * scale) as f32,
            )
        };
        let host = transform(&Rect::new(
            (-HOST_W / 2.0) as f32,
            (-HOST_H / 2.0) as f32,
            HOST_W as f32,
            HOST_H as f32,
        ));
        let screens = base_rects
            .into_iter()
            .map(|(index, position, rect)| (index, position, transform(&rect)))
            .collect();

        Layout {
            host,
            screens,
            scale,
        }
    }

    fn layout_rects(&self, width: f64, height: f64) -> Vec<(usize, Position, Rect)> {
        self.layout(width, height).screens
    }

    fn on_drag_begin(&self, x: f64, y: f64) {
        let obj = self.obj();
        let hit = self.hit_test(x, y);
        self.dragging.set(hit);
        if let Some(index) = hit {
            if let Some(item) = self.items.borrow().get(index) {
                self.selected.set(Some(item.handle));
            }
            obj.grab_focus();
            obj.set_cursor_from_name(Some("grabbing"));
        }
        self.drag_start.set((x, y));
        self.drag_offset.set((0.0, 0.0));
        self.drop_position.set(None);
        obj.queue_draw();
    }

    fn on_drag_update(&self, offset_x: f64, offset_y: f64) {
        if self.dragging.get().is_none() {
            return;
        }
        self.drag_offset.set((offset_x, offset_y));
        let (start_x, start_y) = self.drag_start.get();
        self.drop_position.set(Some(nearest_position(
            start_x + offset_x,
            start_y + offset_y,
            self.obj().width() as f64,
            self.obj().height() as f64,
        )));
        self.obj().queue_draw();
    }

    fn on_drag_end(&self, offset_x: f64, offset_y: f64) {
        let Some(index) = self.dragging.take() else {
            return;
        };
        self.drag_offset.set((0.0, 0.0));
        let drop_position = self.drop_position.take();
        let obj = self.obj();
        let distance = offset_x.hypot(offset_y);
        let hit = self.hit_test(
            self.drag_start.get().0 + offset_x,
            self.drag_start.get().1 + offset_y,
        );
        self.hovered.set(hit);
        obj.set_cursor_from_name(hit.map(|_| "grab"));
        if distance < MIN_DRAG_DISTANCE {
            obj.queue_draw();
            return;
        }
        let Some(position) = drop_position else {
            return;
        };
        let mut items = self.items.borrow_mut();
        let Some(item) = items.get_mut(index) else {
            return;
        };
        let handle = item.handle;
        let changed = item.position != position;
        item.position = position;
        drop(items);
        obj.queue_draw();
        if changed {
            obj.emit_by_name::<()>("position-changed", &[&handle, &position.to_string()]);
        }
    }

    fn hit_test(&self, x: f64, y: f64) -> Option<usize> {
        let items = self.items.borrow();
        let selected = self.selected.get();
        let mut screens = self.layout_rects(self.obj().width() as f64, self.obj().height() as f64);
        screens.sort_by_key(|(index, ..)| {
            items
                .get(*index)
                .is_some_and(|item| selected == Some(item.handle))
        });
        screens
            .into_iter()
            .rev()
            .find(|(_, _, rect)| rect.contains_point(&Point::new(x as f32, y as f32)))
            .map(|(index, ..)| index)
    }

    fn on_motion(&self, x: f64, y: f64) {
        if self.dragging.get().is_some() {
            return;
        }
        let hovered = self.hit_test(x, y);
        if self.hovered.replace(hovered) != hovered {
            self.obj().queue_draw();
        }
        self.obj().set_cursor_from_name(hovered.map(|_| "grab"));
    }

    fn on_leave(&self) {
        if self.dragging.get().is_none() {
            self.hovered.set(None);
            self.obj().set_cursor_from_name(None);
            self.obj().queue_draw();
        }
    }
}

fn nearest_position(x: f64, y: f64, width: f64, height: f64) -> Position {
    let dx = x - width / 2.0;
    let dy = y - height / 2.0;
    if dx.abs() > dy.abs() {
        if dx > 0.0 {
            Position::Right
        } else {
            Position::Left
        }
    } else if dy > 0.0 {
        Position::Bottom
    } else {
        Position::Top
    }
}

fn drop_zone_rect(layout: &Layout, position: Position) -> Rect {
    let gap = GAP * layout.scale;
    let width = SAT_W * layout.scale;
    let height = SAT_H * layout.scale;
    let host = &layout.host;
    let (x, y) = match position {
        Position::Left => (
            host.x() as f64 - gap - width,
            host.y() as f64 + (host.height() as f64 - height) / 2.0,
        ),
        Position::Right => (
            (host.x() + host.width()) as f64 + gap,
            host.y() as f64 + (host.height() as f64 - height) / 2.0,
        ),
        Position::Top => (
            host.x() as f64 + (host.width() as f64 - width) / 2.0,
            host.y() as f64 - gap - height,
        ),
        Position::Bottom => (
            host.x() as f64 + (host.width() as f64 - width) / 2.0,
            (host.y() + host.height()) as f64 + gap,
        ),
    };
    Rect::new(x as f32, y as f32, width as f32, height as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_position_uses_the_dominant_axis() {
        assert_eq!(nearest_position(10.0, 100.0, 400.0, 200.0), Position::Left);
        assert_eq!(
            nearest_position(390.0, 100.0, 400.0, 200.0),
            Position::Right
        );
        assert_eq!(nearest_position(200.0, 5.0, 400.0, 200.0), Position::Top);
        assert_eq!(
            nearest_position(200.0, 195.0, 400.0, 200.0),
            Position::Bottom
        );
    }
}
