use astera_core::{OutputId, Point, Size};
use smithay::{
    desktop::utils::bbox_from_surface_tree,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point as SmithayPoint, Rectangle},
    wayland::input_method::PopupSurface,
};

use super::{Astera, physical_point};
use crate::state::model::MappedInputMethodPopup;

impl Astera {
    pub(super) fn is_input_method_popup_surface(&self, surface: &WlSurface) -> bool {
        self.input_method_popups
            .iter()
            .any(|popup| popup.surface.wl_surface() == surface)
    }

    pub(super) fn reposition_input_method_popup_surface(&mut self, surface: &WlSurface) {
        let popup = self
            .input_method_popups
            .iter()
            .find(|popup| popup.surface.wl_surface() == surface)
            .map(|popup| popup.surface.clone());
        if let Some(popup) = popup {
            self.reposition_input_method_popup(&popup);
        }
    }

    pub(super) fn add_input_method_popup(&mut self, surface: PopupSurface) {
        self.input_method_popups
            .retain(|popup| popup.surface.alive() && popup.surface != surface);
        self.reposition_input_method_popup(&surface);
        self.input_method_popups
            .push(MappedInputMethodPopup { surface });
        self.mark_render_dirty();
        self.refresh_visible_scales();
    }

    pub(super) fn remove_input_method_popup(&mut self, surface: &PopupSurface) {
        self.input_method_popups
            .retain(|popup| popup.surface != *surface);
        self.mark_render_dirty();
        self.refresh_visible_scales();
    }

    pub(super) fn reposition_input_method_popup(&mut self, surface: &PopupSurface) {
        let Some(parent) = surface.get_parent() else {
            return;
        };
        let Some((output, parent_geometry)) = self.input_method_parent_geometry(&parent.surface)
        else {
            return;
        };
        let Some(output_state) = self.desktop.outputs.get(&output) else {
            return;
        };
        let cursor = surface.text_input_rectangle();
        let popup_size = bbox_from_surface_tree(surface.wl_surface(), (0, 0)).size;
        let viewport = output_state.output.logical_size;

        let location = popup_location(parent_geometry, cursor, popup_size, viewport);
        // PopupSurface expects a position relative to its parent surface.
        surface.set_location(location - parent_geometry.loc);
        self.mark_render_dirty();
    }

    pub(super) fn input_method_parent_geometry(
        &self,
        parent: &WlSurface,
    ) -> Option<(OutputId, Rectangle<i32, Logical>)> {
        for output in self.desktop.outputs.keys() {
            if let Some(lock) = self.lock_surface_for_output(*output)
                && lock.wl_surface() == parent
            {
                let size = self.desktop.outputs[output].output.logical_size;
                return Some((
                    *output,
                    Rectangle::new(
                        (0, 0).into(),
                        (size.width as i32, size.height as i32).into(),
                    ),
                ));
            }
            // Never derive or render a desktop-relative IME popup while the lock scene is active.
            // This remains fail-closed even if an input-method client races keyboard focus loss.
            if self.session_is_locked() {
                continue;
            }
            for window in self.windows.iter().filter(|window| window.mapped) {
                if window.surface.wl_surface() == parent
                    && let Some((origin, size, _, _)) =
                        self.visual_geometry_for_output(*output, window.id)
                {
                    return Some((*output, core_rectangle(origin, size)));
                }
            }
            for layer in self
                .layers
                .iter()
                .filter(|layer| layer.mapped && layer.output == *output)
            {
                if layer.surface.wl_surface() == parent
                    && let Some((origin, size)) = self.layer_geometry(layer)
                {
                    return Some((*output, core_rectangle(origin, size)));
                }
            }
        }
        None
    }

    pub(super) fn input_method_roots(
        &self,
        output: OutputId,
    ) -> Vec<(WlSurface, SmithayPoint<i32, Physical>, f64)> {
        let scale = self.output_scale(output);
        self.input_method_popups
            .iter()
            .filter(|popup| popup.surface.alive())
            .filter_map(|popup| {
                let parent = popup.surface.get_parent()?;
                let (parent_output, parent_geometry) =
                    self.input_method_parent_geometry(&parent.surface)?;
                (parent_output == output).then(|| {
                    let location = Point::new(
                        i64::from(parent_geometry.loc.x + popup.surface.location().x),
                        i64::from(parent_geometry.loc.y + popup.surface.location().y),
                    );
                    (
                        popup.surface.wl_surface().clone(),
                        physical_point(location, scale),
                        scale,
                    )
                })
            })
            .collect()
    }

    pub(super) fn input_method_popup_origins(&self, output: OutputId) -> Vec<(WlSurface, Point)> {
        self.input_method_popups
            .iter()
            .filter(|popup| popup.surface.alive())
            .filter_map(|popup| {
                let parent = popup.surface.get_parent()?;
                let (parent_output, parent_geometry) =
                    self.input_method_parent_geometry(&parent.surface)?;
                (parent_output == output).then(|| {
                    (
                        popup.surface.wl_surface().clone(),
                        Point::new(
                            i64::from(parent_geometry.loc.x + popup.surface.location().x),
                            i64::from(parent_geometry.loc.y + popup.surface.location().y),
                        ),
                    )
                })
            })
            .collect()
    }
}

fn popup_location(
    parent: Rectangle<i32, Logical>,
    cursor: Rectangle<i32, Logical>,
    popup_size: smithay::utils::Size<i32, Logical>,
    viewport: Size,
) -> smithay::utils::Point<i32, Logical> {
    let mut x = parent.loc.x.saturating_add(cursor.loc.x);
    let mut y = parent
        .loc
        .y
        .saturating_add(cursor.loc.y)
        .saturating_add(cursor.size.h);
    if y.saturating_add(popup_size.h) > viewport.height as i32 {
        y = parent
            .loc
            .y
            .saturating_add(cursor.loc.y)
            .saturating_sub(popup_size.h);
    }
    x = x.clamp(0, (viewport.width as i32 - popup_size.w).max(0));
    y = y.clamp(0, (viewport.height as i32 - popup_size.h).max(0));
    (x, y).into()
}

fn core_rectangle(origin: Point, size: Size) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (origin.x as i32, origin.y as i32).into(),
        (size.width as i32, size.height as i32).into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_flips_above_and_clamps_to_complete_viewport() {
        let parent = Rectangle::new((100, 50).into(), (400, 300).into());
        let cursor = Rectangle::new((350, 270).into(), (2, 20).into());
        let location = popup_location(parent, cursor, (200, 100).into(), Size::new(640, 360));
        assert_eq!(location, (440, 220).into());

        let oversized = popup_location(parent, cursor, (800, 500).into(), Size::new(640, 360));
        assert_eq!(oversized, (0, 0).into());
    }
}
