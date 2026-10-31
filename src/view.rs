use std::cell::{Cell, RefCell};

use gtk::{gdk, glib, graphene, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    pub struct CastView {
        pub paintable: RefCell<Option<gdk::Paintable>>,
        pub handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub fill: Cell<bool>,
        pub pan: Cell<f64>,
    }

    impl Default for CastView {
        fn default() -> Self {
            Self {
                paintable: RefCell::new(None),
                handlers: RefCell::new(Vec::new()),
                fill: Cell::new(true),
                pan: Cell::new(0.5),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CastView {
        const NAME: &'static str = "ScreencastMirrorView";
        type Type = super::CastView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CastView {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: std::sync::LazyLock<Vec<glib::ParamSpec>> =
                std::sync::LazyLock::new(|| {
                    vec![glib::ParamSpecObject::builder::<gdk::Paintable>("paintable").build()]
                });
            &PROPERTIES
        }

        fn property(&self, _: usize, _: &glib::ParamSpec) -> glib::Value {
            self.paintable.borrow().to_value()
        }

        fn set_property(&self, _: usize, value: &glib::Value, _: &glib::ParamSpec) {
            self.disconnect_paintable();
            let paintable = value.get::<Option<gdk::Paintable>>().unwrap();
            if let Some(ref paintable) = paintable {
                let weak = self.obj().downgrade();
                let changed = paintable.connect_invalidate_contents(move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.queue_draw();
                    }
                });
                let weak = self.obj().downgrade();
                let resized = paintable.connect_invalidate_size(move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.queue_draw();
                    }
                });
                self.handlers.borrow_mut().extend([changed, resized]);
            }
            *self.paintable.borrow_mut() = paintable;
            self.obj().queue_draw();
        }

        fn dispose(&self) {
            self.disconnect_paintable();
        }
    }

    impl CastView {
        fn disconnect_paintable(&self) {
            if let Some(paintable) = self.paintable.borrow_mut().take() {
                for handler in self.handlers.borrow_mut().drain(..) {
                    paintable.disconnect(handler);
                }
            }
        }
    }

    impl WidgetImpl for CastView {
        fn measure(&self, orientation: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            (
                0,
                if orientation == gtk::Orientation::Horizontal {
                    640
                } else {
                    360
                },
                -1,
                -1,
            )
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let paintable = self.paintable.borrow();
            let Some(paintable) = paintable.as_ref() else {
                return;
            };
            let view = self.obj();
            let width = f64::from(view.width());
            let height = f64::from(view.height());
            let source_width = f64::from(paintable.intrinsic_width());
            let source_height = f64::from(paintable.intrinsic_height());
            let Some((x, y, w, h)) = frame_rect(
                width,
                height,
                source_width,
                source_height,
                self.fill.get(),
                self.pan.get(),
            ) else {
                return;
            };
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, width as f32, height as f32));
            snapshot.translate(&graphene::Point::new(x as f32, y as f32));
            paintable.snapshot(snapshot, w, h);
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct CastView(ObjectSubclass<imp::CastView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CastView {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("hexpand", true)
            .property("vexpand", true)
            .build()
    }

    pub fn paintable(&self) -> Option<gdk::Paintable> {
        self.imp().paintable.borrow().clone()
    }

    pub fn set_paintable(&self, paintable: Option<&gdk::Paintable>) {
        self.set_property("paintable", paintable);
    }

    pub fn connect_paintable_notify<F: Fn(&Self) + 'static>(&self, f: F) {
        self.connect_notify_local(Some("paintable"), move |view, _| f(view));
    }

    pub fn set_fill(&self, fill: bool) {
        self.imp().fill.set(fill);
        self.queue_draw();
    }

    pub fn set_pan(&self, pan: f64) {
        self.imp().pan.set(pan.clamp(0.0, 1.0));
        self.queue_draw();
    }
}

impl Default for CastView {
    fn default() -> Self {
        Self::new()
    }
}

// The slider follows whichever axis is cropped; Fit always stays centered.
fn frame_rect(
    vw: f64,
    vh: f64,
    sw: f64,
    sh: f64,
    fill: bool,
    pan: f64,
) -> Option<(f64, f64, f64, f64)> {
    if [vw, vh, sw, sh].iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return None;
    }
    let scale = if fill {
        (vw / sw).max(vh / sh)
    } else {
        (vw / sw).min(vh / sh)
    };
    let (width, height) = (sw * scale, sh * scale);
    let position = if fill { pan.clamp(0.0, 1.0) } else { 0.5 };
    Some((
        (vw - width) * position,
        (vh - height) * position,
        width,
        height,
    ))
}

#[cfg(test)]
mod tests {
    use super::frame_rect;

    #[test]
    fn fit_shows_the_whole_target_centered() {
        assert_eq!(
            frame_rect(600., 600., 1200., 600., false, 0.),
            Some((0., 150., 600., 300.))
        );
    }

    #[test]
    fn fill_pans_horizontally_without_stretching() {
        assert_eq!(
            frame_rect(600., 600., 1200., 600., true, 0.),
            Some((0., 0., 1200., 600.))
        );
        assert_eq!(
            frame_rect(600., 600., 1200., 600., true, 0.5),
            Some((-300., 0., 1200., 600.))
        );
        assert_eq!(
            frame_rect(600., 600., 1200., 600., true, 1.),
            Some((-600., 0., 1200., 600.))
        );
    }

    #[test]
    fn fill_pans_vertically_for_a_tall_target() {
        assert_eq!(
            frame_rect(600., 600., 600., 1200., true, 1.),
            Some((0., -600., 600., 1200.))
        );
    }

    #[test]
    fn ignores_frames_without_a_size() {
        assert!(frame_rect(600., 600., 0., 0., true, 0.5).is_none());
    }

    #[test]
    fn matching_aspect_ratio_needs_no_crop_in_either_mode() {
        for fill in [false, true] {
            assert_eq!(
                frame_rect(800., 400., 1200., 600., fill, 1.),
                Some((0., 0., 800., 400.))
            );
        }
    }

    #[test]
    fn pan_stops_at_the_target_edges() {
        assert_eq!(
            frame_rect(600., 600., 1200., 600., true, -1.),
            Some((0., 0., 1200., 600.))
        );
        assert_eq!(
            frame_rect(600., 600., 1200., 600., true, 2.),
            Some((-600., 0., 1200., 600.))
        );
    }
}
