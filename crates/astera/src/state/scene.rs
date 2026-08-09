use super::*;

impl Astera {
    pub(super) fn surface_under(
        &self,
        location: SmithayPoint<f64, smithay::utils::Logical>,
    ) -> Option<(
        WlSurface,
        SmithayPoint<f64, smithay::utils::Logical>,
        Option<WindowId>,
    )> {
        if self.session_is_locked() {
            let surface = self.lock_surface_for_output(self.active_output)?;
            let (hit, offset) = under_from_surface_tree(
                surface.wl_surface(),
                location,
                (0, 0),
                WindowSurfaceType::ALL,
            )?;
            return Some((hit, (f64::from(offset.x), f64::from(offset.y)).into(), None));
        }
        // Input-method candidate popups render above every normal desktop layer and must consume
        // pointer/touch input instead of allowing clicks to pass through to the application.
        for (popup, origin) in self
            .input_method_popup_origins(self.active_output)
            .into_iter()
            .rev()
        {
            let local =
                SmithayPoint::from((location.x - origin.x as f64, location.y - origin.y as f64));
            if let Some((surface, offset)) =
                under_from_surface_tree(&popup, local, (0, 0), WindowSurfaceType::ALL)
            {
                return Some((
                    surface,
                    (
                        origin.x as f64 + f64::from(offset.x),
                        origin.y as f64 + f64::from(offset.y),
                    )
                        .into(),
                    None,
                ));
            }
        }
        // Hit testing mirrors render stacking. We retain every hit and choose the highest tuple so
        // popups, floating/fullscreen windows and layer surfaces resolve deterministically.
        let mut candidates = Vec::new();
        for (index, mapped) in self.windows.iter().enumerate() {
            let Some((origin, size, scale, mode)) = self.visual_geometry(mapped.id) else {
                continue;
            };
            if point_inside(location, origin, size, scale) {
                let local = SmithayPoint::from((
                    (location.x - origin.x as f64) / scale,
                    (location.y - origin.y as f64) / scale,
                ));
                // Smithay performs input-region and subsurface-aware testing in surface-local
                // coordinates; the returned offset is transformed back to compositor space.
                if let Some((surface, offset)) = under_from_surface_tree(
                    mapped.surface.wl_surface(),
                    local,
                    (0, 0),
                    WindowSurfaceType::ALL,
                ) {
                    candidates.push((
                        (mode_layer(mode), index, 0usize),
                        surface,
                        (
                            origin.x as f64 + f64::from(offset.x) * scale,
                            origin.y as f64 + f64::from(offset.y) * scale,
                        )
                            .into(),
                        Some(mapped.id),
                    ));
                }
            }
            for (popup_index, (popup, popup_offset)) in
                PopupManager::popups_for_surface(mapped.surface.wl_surface()).enumerate()
            {
                let geometry = popup.geometry();
                let popup_origin = Point::new(
                    origin.x + ((popup_offset.x - geometry.loc.x) as f64 * scale).round() as i64,
                    origin.y + ((popup_offset.y - geometry.loc.y) as f64 * scale).round() as i64,
                );
                let popup_size = Size::new(i64::from(geometry.size.w), i64::from(geometry.size.h));
                if point_inside(location, popup_origin, popup_size, scale) {
                    let local = SmithayPoint::from((
                        (location.x - popup_origin.x as f64) / scale,
                        (location.y - popup_origin.y as f64) / scale,
                    ));
                    if let Some((surface, offset)) = under_from_surface_tree(
                        popup.wl_surface(),
                        local,
                        (0, 0),
                        WindowSurfaceType::ALL,
                    ) {
                        candidates.push((
                            (mode_layer(mode), index, popup_index + 1),
                            surface,
                            (
                                popup_origin.x as f64 + f64::from(offset.x) * scale,
                                popup_origin.y as f64 + f64::from(offset.y) * scale,
                            )
                                .into(),
                            Some(mapped.id),
                        ));
                    }
                }
            }
        }
        for (index, mapped) in self.layers.iter().enumerate() {
            if !mapped.mapped || mapped.output != self.active_output {
                continue;
            }
            let Some((origin, size)) = self.layer_geometry(mapped) else {
                continue;
            };
            let order = layer_rank(mapped.layer);
            if point_inside(location, origin, size, 1.0) {
                let local = SmithayPoint::from((
                    location.x - origin.x as f64,
                    location.y - origin.y as f64,
                ));
                if let Some((surface, offset)) = under_from_surface_tree(
                    mapped.surface.wl_surface(),
                    local,
                    (0, 0),
                    WindowSurfaceType::ALL,
                ) {
                    candidates.push((
                        (order, index, 0),
                        surface,
                        (
                            origin.x as f64 + f64::from(offset.x),
                            origin.y as f64 + f64::from(offset.y),
                        )
                            .into(),
                        None,
                    ));
                }
            }
            for (popup_index, (popup, popup_offset)) in
                PopupManager::popups_for_surface(mapped.surface.wl_surface()).enumerate()
            {
                let geometry = popup.geometry();
                let popup_origin = Point::new(
                    origin.x + i64::from(popup_offset.x - geometry.loc.x),
                    origin.y + i64::from(popup_offset.y - geometry.loc.y),
                );
                let popup_size = Size::new(i64::from(geometry.size.w), i64::from(geometry.size.h));
                if point_inside(location, popup_origin, popup_size, 1.0) {
                    let local = SmithayPoint::from((
                        location.x - popup_origin.x as f64,
                        location.y - popup_origin.y as f64,
                    ));
                    if let Some((surface, offset)) = under_from_surface_tree(
                        popup.wl_surface(),
                        local,
                        (0, 0),
                        WindowSurfaceType::ALL,
                    ) {
                        candidates.push((
                            (order, index, popup_index + 1),
                            surface,
                            (
                                popup_origin.x as f64 + f64::from(offset.x),
                                popup_origin.y as f64 + f64::from(offset.y),
                            )
                                .into(),
                            None,
                        ));
                    }
                }
            }
        }
        candidates
            .into_iter()
            .max_by_key(|(order, _, _, _)| *order)
            .map(|(_, surface, origin, id)| (surface, origin, id))
    }

    pub(super) fn visual_geometry(&self, id: WindowId) -> Option<(Point, Size, f64, WindowMode)> {
        self.visual_geometry_for_output(self.active_output, id)
    }

    pub(super) fn visual_geometry_for_output(
        &self,
        output_id: OutputId,
        id: WindowId,
    ) -> Option<(Point, Size, f64, WindowMode)> {
        let output = self.desktop.outputs.get(&output_id)?;
        let usable = self.usable_rect(output_id)?;
        let workspace = self.desktop.workspace_for_output(output_id)?;
        let mode = workspace.window_mode(id)?;
        match mode {
            WindowMode::Tiled => {
                let mut rect = workspace.tiled[&id].geometry;
                if let Some(drag) = self
                    .drag
                    .filter(|drag| drag.window == id && output_id == self.active_output)
                {
                    rect.origin = drag.target;
                }
                let left = workspace.camera.center.x as f64 - usable.size.width as f64 / 2.0;
                let top = workspace.camera.center.y as f64 - usable.size.height as f64 / 2.0;
                Some((
                    Point::new(
                        usable.origin.x + (rect.origin.x as f64 - left).round() as i64,
                        usable.origin.y + (rect.origin.y as f64 - top).round() as i64,
                    ),
                    rect.size,
                    1.0,
                    mode,
                ))
            }
            WindowMode::Floating => {
                let mut rect = workspace.floating[&id].viewport.rect;
                if let Some(drag) = self
                    .drag
                    .filter(|drag| drag.window == id && output_id == self.active_output)
                {
                    rect.origin = drag.target;
                }
                Some((rect.origin, rect.size, 1.0, mode))
            }
            WindowMode::Maximized => Some((usable.origin, usable.size, 1.0, mode)),
            WindowMode::Fullscreen => Some((Point::ORIGIN, output.output.logical_size, 1.0, mode)),
        }
    }
}
