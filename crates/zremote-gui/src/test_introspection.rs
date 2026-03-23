//! UI element introspection for E2E GUI testing.
//!
//! When the `test-introspection` feature is enabled, views register their element
//! bounds via [`track()`]. An HTTP server exposes the collected snapshot so that
//! external test harnesses can query element positions.
//!
//! When the feature is disabled, [`track()`] compiles to a no-op.

#[cfg(feature = "test-introspection")]
mod inner {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    use gpui::{Bounds, Global, Pixels};
    use serde::Serialize;

    /// Snapshot of element positions, shared between GPUI thread and HTTP server.
    #[derive(Default, Clone, Serialize)]
    pub struct ElementSnapshot {
        pub generation: u64,
        pub elements: HashMap<String, ElementBounds>,
    }

    /// Bounding rectangle of a tracked UI element.
    #[derive(Clone, Serialize)]
    pub struct ElementBounds {
        pub x: f32,
        pub y: f32,
        pub w: f32,
        pub h: f32,
        pub visible: bool,
    }

    /// GPUI Global that collects element bounds during paint.
    ///
    /// Views call [`ElementRegistry::register`] during render, then [`flush`] at
    /// the end of the frame to publish the snapshot for the HTTP server.
    pub struct ElementRegistry {
        elements: HashMap<String, (Bounds<Pixels>, u64)>,
        frame_generation: u64,
        shared: Arc<RwLock<ElementSnapshot>>,
    }

    impl Global for ElementRegistry {}

    impl ElementRegistry {
        pub fn new(shared: Arc<RwLock<ElementSnapshot>>) -> Self {
            Self {
                elements: HashMap::new(),
                frame_generation: 0,
                shared,
            }
        }

        /// Called at the start of each render cycle.
        pub fn begin_frame(&mut self) {
            self.frame_generation += 1;
        }

        /// Register an element's bounds during paint/render.
        pub fn register(&mut self, id: String, bounds: Bounds<Pixels>) {
            self.elements.insert(id, (bounds, self.frame_generation));
        }

        /// Flush the current state to the shared snapshot (called at end of render).
        pub fn flush(&mut self) {
            let current_gen = self.frame_generation;
            let elements: HashMap<String, ElementBounds> = self
                .elements
                .iter()
                .map(|(id, (bounds, frame))| {
                    (
                        id.clone(),
                        ElementBounds {
                            x: f32::from(bounds.origin.x),
                            y: f32::from(bounds.origin.y),
                            w: f32::from(bounds.size.width),
                            h: f32::from(bounds.size.height),
                            visible: *frame == current_gen,
                        },
                    )
                })
                .collect();

            if let Ok(mut snapshot) = self.shared.write() {
                *snapshot = ElementSnapshot {
                    generation: current_gen,
                    elements,
                };
            }
        }
    }

    /// Shared snapshot type alias for convenience.
    pub type SharedSnapshot = Arc<RwLock<ElementSnapshot>>;

    /// Register an element's bounds in the introspection registry.
    ///
    /// Safe to call unconditionally -- no-ops if the registry global is not set.
    pub fn track(cx: &mut gpui::App, id: &str, bounds: Bounds<Pixels>) {
        if cx.has_global::<ElementRegistry>() {
            cx.global_mut::<ElementRegistry>()
                .register(id.to_string(), bounds);
        }
    }
}

#[cfg(feature = "test-introspection")]
pub use inner::*;

/// No-op stub when the `test-introspection` feature is disabled.
#[cfg(not(feature = "test-introspection"))]
pub fn track(_cx: &mut gpui::App, _id: &str, _bounds: gpui::Bounds<gpui::Pixels>) {}
