//! A container that shows its one child whole or not at all. It asks for no
//! minimum width and only asks for the child's natural width while `row`
//! (an ancestor laying out siblings) has room for every visible child at
//! natural width, so on a narrow window it gives its space back instead of
//! squeezing the siblings. It does not expand; its `spacer` takes the row's
//! spare width instead and has it re-decide whenever that width changes.

use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Default)]
    pub struct FitOrHide {
        pub row: glib::WeakRef<gtk::Widget>,
        pub child: RefCell<Option<gtk::Widget>>,
        /// Whether the child fits at natural width: then it is measured and drawn.
        pub fits: Cell<bool>,
        /// Whether the latest allocation was wide enough to draw it.
        pub drawn: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FitOrHide {
        const NAME: &'static str = "OcgtkFitOrHide";
        type Type = super::FitOrHide;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for FitOrHide {
        fn constructed(&self) {
            self.parent_constructed();
            self.fits.set(true);
        }

        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl FitOrHide {
        /// Whether the child fits the row at natural width; on a change, the
        /// widget is resized on the next frame.
        pub fn decide(&self) -> bool {
            let Some(child) = self.child.borrow().clone() else {
                return false;
            };
            let natural = child.measure(gtk::Orientation::Horizontal, -1).1;
            let obj = self.obj();
            let fits = self.row.upgrade().is_none_or(|row| {
                // What the row counts for this widget now: nothing while hidden.
                let reported = if obj.is_visible() && self.fits.get() {
                    natural
                } else {
                    0
                };
                let needed = row.measure(gtk::Orientation::Horizontal, -1).1 - reported + natural;
                row.width() >= needed
            });
            if fits != self.fits.get() {
                self.fits.set(fits);
                // Not from inside the allocation: resize on the next frame.
                let weak = obj.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(widget) = weak.upgrade() {
                        widget.queue_resize();
                    }
                });
            }
            fits
        }
    }

    impl WidgetImpl for FitOrHide {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            if orientation == gtk::Orientation::Horizontal {
                let natural = child.measure(orientation, -1).1;
                (0, if self.fits.get() { natural } else { 0 }, -1, -1)
            } else {
                let (minimum, natural, _, _) = child.measure(orientation, for_size);
                (minimum, natural, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            let (minimum, natural, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            child.size_allocate(
                &gtk::Allocation::new(0, 0, width.min(natural).max(minimum), height),
                baseline,
            );
            let fits = self.decide();
            self.drawn.set(fits && width >= natural);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if !self.drawn.get() {
                return;
            }
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct FitOrHide(ObjectSubclass<imp::FitOrHide>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FitOrHide {
    /// `row` is the ancestor whose natural width, with the child in it, must
    /// fit its allocated width.
    pub fn new(child: &impl IsA<gtk::Widget>, row: &impl IsA<gtk::Widget>) -> Self {
        let this: Self = glib::Object::new();
        child.set_parent(&this);
        this.imp().child.replace(Some(child.clone().upcast()));
        this.imp().row.set(Some(row.upcast_ref()));
        this.set_can_target(false);
        this
    }

    /// An empty widget for the row's flexible space. Expanding, it is
    /// re-allocated whenever the row's width changes, and has this re-decide
    /// then, even while this asks for no width and so is not re-allocated.
    pub fn spacer(&self) -> gtk::Widget {
        let spacer = gtk::DrawingArea::new();
        spacer.set_hexpand(true);
        spacer.set_can_target(false);
        let weak = self.downgrade();
        spacer.connect_resize(move |_, _, _| {
            if let Some(this) = weak.upgrade() {
                this.imp().decide();
            }
        });
        spacer.upcast()
    }
}
