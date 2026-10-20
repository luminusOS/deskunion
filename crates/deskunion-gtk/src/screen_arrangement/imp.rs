use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use adw::subclass::prelude::*;
use gtk::glib::clone;
use gtk::glib::subclass::Signal;
use gtk::graphene::{Point, Rect};
use gtk::{gdk, glib, gsk, prelude::*};

use deskunion_ipc::Position;

use super::ScreenItem;

// 16:10 tiles, like the monitors in GNOME Settings → Displays
const HOST_W: f64 = 104.0;
const HOST_H: f64 = 65.0;
const SAT_W: f64 = 88.0;
const SAT_H: f64 = 55.0;
const GAP: f64 = 18.0;
const CANVAS_PADDING: f64 = 16.0;
const MIN_DRAG_DISTANCE: f64 = 6.0;
const KEYBOARD_HELP: &str =
    "Page Up/Down or Home/End choose a device; arrow keys move it; Escape cancels a drag.";
/// perpendicular offset applied to the 2nd, 3rd, ... screen stacked at
/// the same border (only possible for *configured*, not *active*,
/// clients — the protocol allows one active client per border)
const EDGE_GAP: f64 = 8.0;
const CORNER_RADIUS: f32 = 8.0;
const BADGE_RADIUS: f64 = 5.0;

struct Layout {
    host: Rect,
    screens: Vec<(usize, Position, Rect)>,
    scale: f64,
}

struct ScreenStyle<'a> {
    /// 1-based position shown in the tile, matching the client list below
    number: usize,
    badge_text: &'a gdk::RGBA,
    fill: &'a gdk::RGBA,
    text: &'a gdk::RGBA,
    success: &'a gdk::RGBA,
    warning: &'a gdk::RGBA,
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
    dragging: Cell<Option<u64>>,
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
        obj.set_tooltip_text(Some(&format!(
            "Drag a device to choose its screen edge. {KEYBOARD_HELP}"
        )));
        obj.update_property(&[gtk::accessible::Property::Label("Device arrangement")]);
        self.update_accessibility();

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = widget)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_keys, key, _code, _state| widget.on_key(key)
        ));
        obj.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(clone!(
            #[weak(rename_to = widget)]
            self,
            move |_| widget.obj().queue_draw()
        ));
        focus.connect_leave(clone!(
            #[weak(rename_to = widget)]
            self,
            move |_| widget.obj().queue_draw()
        ));
        obj.add_controller(focus);

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
    fn request_mode(&self) -> gtk::SizeRequestMode {
        gtk::SizeRequestMode::HeightForWidth
    }

    fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
        let positions = self
            .items
            .borrow()
            .iter()
            .map(|item| item.position)
            .collect::<Vec<_>>();
        let (min_x, max_x, min_y, max_y) = geometry_bounds(&positions, self.card_scale());
        let width = max_x - min_x + CANVAS_PADDING * 2.0;
        if orientation == gtk::Orientation::Horizontal {
            (280, width.ceil().max(280.0) as i32, -1, -1)
        } else {
            let fit = if for_size > 0 {
                ((for_size as f64 - CANVAS_PADDING * 2.0).max(1.0) / (max_x - min_x)).min(1.0)
            } else {
                1.0
            };
            let height = ((max_y - min_y) * fit + CANVAS_PADDING * 2.0).ceil() as i32;
            (height, height, -1, -1)
        }
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
        // `accent_bg_color` is a fill (paired with `accent_fg_color`);
        // `accent_color` is the variant meant for text and borders.
        #[allow(deprecated)]
        let accent_text = style.lookup_color("accent_color").unwrap_or(accent);
        #[allow(deprecated)]
        let warning = style
            .lookup_color("warning_color")
            .unwrap_or(gdk::RGBA::new(0.95, 0.65, 0.15, 1.0));
        #[allow(deprecated)]
        let accent_fg = style
            .lookup_color("accent_fg_color")
            .unwrap_or(gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
        #[allow(deprecated)]
        let success = style
            .lookup_color("success_color")
            .unwrap_or(gdk::RGBA::new(0.2, 0.7, 0.3, 1.0));
        #[allow(deprecated)]
        let card_bg = style
            .lookup_color("window_bg_color")
            .unwrap_or(gdk::RGBA::new(0.2, 0.2, 0.2, 1.0));
        let muted = with_alpha(&fg, 0.08);
        let active = with_alpha(&success, 0.16);
        let dim = with_alpha(&fg, 0.7);
        let layout = self.layout(width, height);
        let widget: &gtk::Widget = obj.upcast_ref();

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
                    &accent_text,
                    position == target,
                    &position.to_string(),
                );
            }
        }

        draw_screen(
            widget,
            snapshot,
            &layout.host,
            &ScreenStyle {
                number: 1,
                badge_text: &accent,
                fill: &accent,
                text: &accent_fg,
                success: &success,
                warning: &warning,
                selection: &accent_text,
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
            dragging
                .is_some_and(|handle| items.get(*index).is_some_and(|item| item.handle == handle))
                || items
                    .get(*index)
                    .is_some_and(|item| selected == Some(item.handle))
        });
        for (i, _, rect) in screens {
            let rect = if dragging
                .is_some_and(|handle| items.get(i).is_some_and(|item| item.handle == handle))
            {
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
            let color = if item.connected { &active } else { &muted };
            draw_screen(
                widget,
                snapshot,
                &rect,
                &ScreenStyle {
                    number: screen_number(i),
                    badge_text: &card_bg,
                    fill: color,
                    text: &fg,
                    success: &success,
                    warning: &warning,
                    selection: &accent_text,
                    audio_active: item.audio_active,
                    selected: selected == Some(item.handle),
                    hovered: hovered == Some(i),
                    active: item.connected,
                },
            );
        }

        if obj.has_focus()
            && obj
                .root()
                .and_downcast::<gtk::Window>()
                .is_some_and(|window| window.property::<bool>("focus-visible"))
        {
            let bounds = Rect::new(2.0, 2.0, (width - 4.0) as f32, (height - 4.0) as f32);
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(bounds, CORNER_RADIUS),
                &[2.0; 4],
                &[accent_text; 4],
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

fn device_name(item: &ScreenItem) -> String {
    item.hostname
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("Unnamed device")
        .to_string()
}

fn connection_status(active: bool, connected: bool) -> &'static str {
    if connected {
        "Connected"
    } else if active {
        "Paired · offline"
    } else {
        "Paused"
    }
}

fn with_alpha(color: &gdk::RGBA, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        color.red(),
        color.green(),
        color.blue(),
        color.alpha() * alpha,
    )
}

fn draw_screen(
    widget: &gtk::Widget,
    snapshot: &gtk::Snapshot,
    rect: &Rect,
    style: &ScreenStyle<'_>,
) {
    let rounded = gsk::RoundedRect::from_rect(*rect, CORNER_RADIUS);
    snapshot.push_rounded_clip(&rounded);
    snapshot.append_color(style.fill, rect);
    snapshot.pop();
    if style.hovered && !style.selected {
        snapshot.push_rounded_clip(&rounded);
        snapshot.append_color(&with_alpha(style.text, 0.08), rect);
        snapshot.pop();
    }

    let (border_width, outline) = if style.selected {
        (2.0, *style.selection)
    } else {
        (1.0, with_alpha(style.text, 0.3))
    };
    snapshot.append_border(&rounded, &[border_width; 4], &[outline; 4]);

    let number = widget.create_pango_layout(Some(&style.number.to_string()));
    let attrs = gtk::pango::AttrList::new();
    attrs.insert(gtk::pango::AttrInt::new_weight(gtk::pango::Weight::Bold));
    number.set_attributes(Some(&attrs));
    let (number_w, number_h) = number.pixel_size();
    let (radius, text_scale) = number_badge_metrics(rect, number_w as f32, number_h as f32);
    attrs.insert(gtk::pango::AttrFloat::new_scale(text_scale as f64));
    number.set_attributes(Some(&attrs));
    let (number_w, number_h) = number.pixel_size();
    let center = Point::new(
        rect.x() + rect.width() / 2.0,
        rect.y() + rect.height() / 2.0,
    );
    let disc = Rect::new(
        center.x() - radius,
        center.y() - radius,
        radius * 2.0,
        radius * 2.0,
    );
    snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(disc, radius));
    snapshot.append_color(style.text, &disc);
    snapshot.pop();
    snapshot.save();
    snapshot.translate(&Point::new(
        center.x() - number_w as f32 / 2.0,
        center.y() - number_h as f32 / 2.0,
    ));
    snapshot.append_layout(&number, style.badge_text);
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
            style.warning,
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
            self.update_accessibility();
        }
    }

    pub(super) fn set_items(&self, items: Vec<ScreenItem>) {
        let geometry_changed = self
            .items
            .borrow()
            .iter()
            .map(|item| item.position)
            .ne(items.iter().map(|item| item.position));
        self.hovered.set(None);
        if reconcile_drag(self.dragging.get(), items.iter().map(|item| item.handle))
            != self.dragging.get()
        {
            self.dragging.set(None);
            self.drag_offset.set((0.0, 0.0));
            self.drop_position.set(None);
            self.obj().set_cursor_from_name(None);
        }
        if self
            .selected
            .get()
            .is_some_and(|handle| !items.iter().any(|item| item.handle == handle))
        {
            self.selected.set(None);
        }
        self.items.replace(items);
        if geometry_changed {
            self.obj().queue_resize();
        }
        self.obj().queue_draw();
        self.update_accessibility();
    }

    /// announces the devices, their edges and the selection to assistive
    /// technology — the canvas is otherwise invisible to them
    fn update_accessibility(&self) {
        let host = self.host_label.borrow();
        let host = if host.is_empty() {
            "This computer"
        } else {
            &host
        };
        let items = self.items.borrow();
        let mut text = if items.is_empty() {
            format!("{host}. No devices to arrange.")
        } else {
            let devices: Vec<String> = items
                .iter()
                .map(|item| {
                    format!(
                        "screen {}, {}, {}, on the {} edge",
                        screen_number(
                            items
                                .iter()
                                .position(|candidate| candidate.handle == item.handle)
                                .unwrap_or(0)
                        ),
                        device_name(item),
                        connection_status(item.active, item.connected).to_lowercase(),
                        item.position
                    )
                })
                .collect();
            format!("{host} with {}.", devices.join(", "))
        };
        if let Some(item) = items
            .iter()
            .find(|item| self.selected.get() == Some(item.handle))
        {
            let number = screen_number(
                items
                    .iter()
                    .position(|candidate| candidate.handle == item.handle)
                    .unwrap_or(0),
            );
            text.push_str(&format!(
                " Selected: screen {number}, {}.",
                device_name(item)
            ));
        }
        if !items.is_empty() {
            text.push(' ');
            text.push_str(KEYBOARD_HELP);
        }
        self.obj()
            .update_property(&[gtk::accessible::Property::Description(&text)]);
    }

    fn card_scale(&self) -> f64 {
        let number = self.obj().create_pango_layout(Some(
            &screen_number(self.items.borrow().len()).max(99).to_string(),
        ));
        let attrs = gtk::pango::AttrList::new();
        attrs.insert(gtk::pango::AttrInt::new_weight(gtk::pango::Weight::Bold));
        number.set_attributes(Some(&attrs));
        let (w, h) = number.pixel_size();
        1.0_f64.max(((w as f64).hypot(h as f64) + 12.0) / (SAT_H - 8.0))
    }

    fn layout(&self, width: f64, height: f64) -> Layout {
        let items = self.items.borrow();
        let positions = items.iter().map(|item| item.position).collect::<Vec<_>>();
        canvas_layout(&positions, width, height, self.card_scale())
    }

    fn layout_rects(&self, width: f64, height: f64) -> Vec<(usize, Position, Rect)> {
        self.layout(width, height).screens
    }

    fn on_drag_begin(&self, x: f64, y: f64) {
        let obj = self.obj();
        let hit = self.hit_test(x, y);
        self.dragging
            .set(hit.and_then(|index| self.items.borrow().get(index).map(|item| item.handle)));
        obj.grab_focus();
        if let Some(index) = hit {
            if let Some(item) = self.items.borrow().get(index) {
                self.selected.set(Some(item.handle));
            }
            obj.set_cursor_from_name(Some("grabbing"));
        } else {
            self.selected.set(None);
        }
        self.update_accessibility();
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
        let Some(handle) = self.dragging.take() else {
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
        self.set_position_by_handle(handle, position);
    }

    fn set_position(&self, index: usize, position: Position) {
        let mut items = self.items.borrow_mut();
        let Some(item) = items.get_mut(index) else {
            return;
        };
        let handle = item.handle;
        let changed = item.position != position;
        item.position = position;
        drop(items);
        let obj = self.obj();
        obj.queue_draw();
        self.update_accessibility();
        if changed {
            obj.queue_resize();
            obj.emit_by_name::<()>("position-changed", &[&handle, &position.to_string()]);
        }
    }

    fn set_position_by_handle(&self, handle: u64, position: Position) {
        let index = self
            .items
            .borrow()
            .iter()
            .position(|item| item.handle == handle);
        if let Some(index) = index {
            self.set_position(index, position);
        }
    }

    /// Page Up/Down, Home/End: choose the selected device; arrow keys:
    /// move it to that edge — the keyboard equivalent of dragging.
    /// Escape cancels a drag in progress.
    fn on_key(&self, key: gdk::Key) -> glib::Propagation {
        if key == gdk::Key::Escape {
            if self.dragging.take().is_none() {
                return glib::Propagation::Proceed;
            }
            self.drag_offset.set((0.0, 0.0));
            self.drop_position.set(None);
            self.obj().set_cursor_from_name(None);
            self.obj().queue_draw();
            return glib::Propagation::Stop;
        }
        let position = match key {
            gdk::Key::Left => Some(Position::Left),
            gdk::Key::Right => Some(Position::Right),
            gdk::Key::Up => Some(Position::Top),
            gdk::Key::Down => Some(Position::Bottom),
            gdk::Key::Page_Up | gdk::Key::Page_Down | gdk::Key::Home | gdk::Key::End => None,
            _ => return glib::Propagation::Proceed,
        };
        let (index, count) = {
            let items = self.items.borrow();
            let selected = self.selected.get();
            (
                items.iter().position(|item| Some(item.handle) == selected),
                items.len(),
            )
        };
        if count == 0 {
            return glib::Propagation::Proceed;
        }
        match (position, index) {
            (Some(position), Some(index)) => self.set_position(index, position),
            _ => {
                let next = match (key, index) {
                    (gdk::Key::Home, _) => 0,
                    (gdk::Key::End, _) => count - 1,
                    (gdk::Key::Page_Up, Some(i)) => (i + count - 1) % count,
                    (gdk::Key::Page_Down, Some(i)) => (i + 1) % count,
                    _ => 0,
                };
                let handle = self.items.borrow().get(next).map(|item| item.handle);
                self.selected.set(handle);
                self.obj().queue_draw();
                self.update_accessibility();
            }
        }
        glib::Propagation::Stop
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

fn screen_number(index: usize) -> usize {
    index + 2
}

fn number_badge_metrics(rect: &Rect, text_w: f32, text_h: f32) -> (f32, f32) {
    let diagonal = text_w.hypot(text_h).max(1.0);
    let radius = (diagonal / 2.0 + 6.0)
        .max(11.0)
        .min((rect.width().min(rect.height()) / 2.0 - 2.0).max(0.5));
    let text_scale = ((radius * 2.0 - 8.0).max(1.0) / diagonal).min(1.0);
    (radius, text_scale)
}

fn geometry_bounds(positions: &[Position], card_scale: f64) -> (f64, f64, f64, f64) {
    let mut min_x = (-HOST_W / 2.0 - GAP - SAT_W) * card_scale;
    let mut max_x = -min_x;
    let mut min_y = (-HOST_H / 2.0 - GAP - SAT_H) * card_scale;
    let mut max_y = -min_y;
    for (_, _, rect) in layout_geometry(positions, card_scale) {
        min_x = min_x.min(rect.x() as f64);
        max_x = max_x.max((rect.x() + rect.width()) as f64);
        min_y = min_y.min(rect.y() as f64);
        max_y = max_y.max((rect.y() + rect.height()) as f64);
    }
    (min_x, max_x, min_y, max_y)
}

fn canvas_layout(positions: &[Position], width: f64, height: f64, card_scale: f64) -> Layout {
    let (min_x, max_x, min_y, max_y) = geometry_bounds(positions, card_scale);
    let scale = ((width - CANVAS_PADDING * 2.0).max(1.0) / (max_x - min_x))
        .min((height - CANVAS_PADDING * 2.0).max(1.0) / (max_y - min_y))
        .min(1.0);
    let translate_x = width / 2.0 - (min_x + max_x) / 2.0 * scale;
    let translate_y = height / 2.0 - (min_y + max_y) / 2.0 * scale;
    let transform = |rect: Rect| {
        Rect::new(
            (rect.x() as f64 * scale + translate_x) as f32,
            (rect.y() as f64 * scale + translate_y) as f32,
            (rect.width() as f64 * scale) as f32,
            (rect.height() as f64 * scale) as f32,
        )
    };
    Layout {
        host: transform(Rect::new(
            (-HOST_W / 2.0 * card_scale) as f32,
            (-HOST_H / 2.0 * card_scale) as f32,
            (HOST_W * card_scale) as f32,
            (HOST_H * card_scale) as f32,
        )),
        screens: layout_geometry(positions, card_scale)
            .into_iter()
            .map(|(i, p, rect)| (i, p, transform(rect)))
            .collect(),
        scale: scale * card_scale,
    }
}

fn layout_geometry(positions: &[Position], card_scale: f64) -> Vec<(usize, Position, Rect)> {
    let (tile_w, tile_h) = (SAT_W * card_scale, SAT_H * card_scale);
    let (host_w, host_h, gap) = (HOST_W * card_scale, HOST_H * card_scale, GAP * card_scale);
    let mut seen = std::collections::HashMap::<Position, usize>::new();
    let totals = positions
        .iter()
        .fold(std::collections::HashMap::new(), |mut totals, p| {
            *totals.entry(*p).or_insert(0usize) += 1;
            totals
        });
    let mut result: Vec<(usize, Position, Rect)> = Vec::new();
    for (index, position) in positions.iter().copied().enumerate() {
        let lane_index = seen.entry(position).or_default();
        let stack = (*lane_index as f64 - (totals[&position].saturating_sub(1)) as f64 / 2.0)
            * (if matches!(position, Position::Left | Position::Right) {
                tile_h
            } else {
                tile_w
            } + EDGE_GAP);
        *lane_index += 1;
        let (mut x, mut y) = match position {
            Position::Left => (-host_w / 2.0 - gap - tile_w, -tile_h / 2.0 + stack),
            Position::Right => (host_w / 2.0 + gap, -tile_h / 2.0 + stack),
            Position::Top => (-tile_w / 2.0 + stack, -host_h / 2.0 - gap - tile_h),
            Position::Bottom => (-tile_w / 2.0 + stack, host_h / 2.0 + gap),
        };
        let mut rect = Rect::new(x as f32, y as f32, tile_w as f32, tile_h as f32);
        let horizontal_lane = matches!(position, Position::Top | Position::Bottom);
        while result
            .iter()
            .any(|(_, _, other)| rectangles_overlap(&rect, other))
        {
            if horizontal_lane {
                x += tile_w + EDGE_GAP;
            } else {
                y += tile_h + EDGE_GAP;
            }
            rect = Rect::new(x as f32, y as f32, tile_w as f32, tile_h as f32);
        }
        result.push((index, position, rect));
    }
    result
}

fn rectangles_overlap(a: &Rect, b: &Rect) -> bool {
    a.x() < b.x() + b.width()
        && b.x() < a.x() + a.width()
        && a.y() < b.y() + b.height()
        && b.y() < a.y() + a.height()
}

fn reconcile_drag(handles: Option<u64>, available: impl IntoIterator<Item = u64>) -> Option<u64> {
    handles.filter(|dragging| available.into_iter().any(|handle| handle == *dragging))
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

    #[test]
    fn paired_device_without_session_is_not_shown_as_connected() {
        assert_eq!(connection_status(true, false), "Paired · offline");
    }

    #[test]
    fn connected_device_is_shown_as_connected() {
        assert_eq!(connection_status(true, true), "Connected");
    }

    #[test]
    fn same_edge_screens_have_non_overlapping_tiles() {
        let edges = [
            Position::Left,
            Position::Right,
            Position::Top,
            Position::Bottom,
        ];
        for left in 1..=3 {
            for right in 1..=3 {
                for top in 1..=3 {
                    for bottom in 1..=3 {
                        let positions = edges
                            .iter()
                            .zip([left, right, top, bottom])
                            .flat_map(|(edge, count)| std::iter::repeat_n(*edge, count))
                            .collect::<Vec<_>>();
                        let rects = layout_geometry(&positions, 1.0);
                        for first in 0..rects.len() {
                            for second in first + 1..rects.len() {
                                assert!(
                                    !rectangles_overlap(&rects[first].2, &rects[second].2),
                                    "overlap for counts {left}/{right}/{top}/{bottom}: {first} and {second}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn dragging_identity_survives_reordering() {
        assert_eq!(reconcile_drag(Some(42), [9, 77, 42]), Some(42));
        assert_eq!(reconcile_drag(Some(42), [9, 77]), None);
    }

    #[test]
    fn displayed_screen_numbers_follow_item_order() {
        assert_eq!((0..3).map(screen_number).collect::<Vec<_>>(), vec![2, 3, 4]);
    }

    #[test]
    fn fitted_layout_and_number_badges_stay_inside_small_canvas() {
        let positions = [
            Position::Top,
            Position::Top,
            Position::Right,
            Position::Left,
            Position::Bottom,
        ];
        for card_scale in [1.0, 1.5, 2.0] {
            let layout = canvas_layout(&positions, 320.0, 160.0, card_scale);
            for rect in
                std::iter::once(&layout.host).chain(layout.screens.iter().map(|(_, _, rect)| rect))
            {
                assert!(rect.x() >= 0.0 && rect.y() >= 0.0);
                assert!(rect.x() + rect.width() <= 320.01);
                assert!(rect.y() + rect.height() <= 160.01);
                let (radius, text_scale) = number_badge_metrics(rect, 40.0, 30.0);
                assert!(radius * 2.0 <= rect.width().min(rect.height()));
                assert!(50.0 * text_scale <= radius * 2.0);
            }
        }
    }
}
