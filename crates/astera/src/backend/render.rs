//! Shared construction of a render-element snapshot.
//!
//! Frame callbacks are captured beside the exact surface that produced an
//! element. Backends complete this immutable list only after presentation.

use smithay::{
    backend::renderer::{
        ImportAll, Renderer,
        element::{Kind, surface::WaylandSurfaceRenderElement},
        utils::RendererSurfaceStateUserData,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Physical, Point, Scale},
    wayland::compositor::{TraversalAction, with_surface_tree_downward},
};

use crate::state::{FrameCallback, frame_callbacks_surface};

pub fn surface_tree_snapshot<R>(
    renderer: &mut R,
    surface: &WlSurface,
    location: Point<i32, Physical>,
    scale: f64,
    alpha: f32,
    kind: Kind,
    callbacks: &mut Vec<FrameCallback>,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let location = location.to_f64();
    let scale = Scale::from(scale);
    let mut elements = Vec::new();
    with_surface_tree_downward(
        surface,
        location,
        |_, states, location| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return TraversalAction::SkipChildren;
            };
            let Some(view) = data.lock().expect("renderer surface state poisoned").view() else {
                return TraversalAction::SkipChildren;
            };
            TraversalAction::DoChildren(*location + view.offset.to_f64().to_physical(scale))
        },
        |surface, states, location| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return;
            };
            let Some(view) = data.lock().expect("renderer surface state poisoned").view() else {
                return;
            };
            let element_location = *location + view.offset.to_f64().to_physical(scale);
            match WaylandSurfaceRenderElement::from_surface(
                renderer,
                surface,
                states,
                element_location,
                alpha,
                kind,
            ) {
                Ok(Some(element)) => {
                    callbacks.extend(frame_callbacks_surface(surface));
                    elements.push(element);
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "could not import surface for frame"),
            }
        },
        |_, _, _| true,
    );
    elements
}
