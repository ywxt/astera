use super::*;

impl Astera {
    fn mapped_windows_for_output(
        &self,
        output: OutputId,
    ) -> impl Iterator<
        Item = (
            &ToplevelSurface,
            SmithayPoint<i32, Physical>,
            f64,
            WindowMode,
        ),
    > {
        let output_scale = self.output_scale(output);
        let mut instances: Vec<_> = self
            .windows
            .iter()
            .filter_map(|mapped| {
                let (origin, _, scale, mode) =
                    self.visual_geometry_for_output(output, mapped.id)?;
                let layer = mode_layer(mode);
                Some((layer, &mapped.surface, origin, scale, mode))
            })
            .collect();
        instances.sort_by_key(|(layer, _, _, _, _)| std::cmp::Reverse(*layer));
        instances
            .into_iter()
            .map(move |(_, surface, origin, scale, mode)| {
                (
                    surface,
                    physical_point(origin, output_scale),
                    scale * output_scale,
                    mode,
                )
            })
    }

    pub fn render_roots(&self) -> Vec<(WlSurface, SmithayPoint<i32, Physical>, f64)> {
        self.render_roots_for_output(self.active_output)
    }

    pub fn render_roots_for_output(
        &self,
        output: OutputId,
    ) -> Vec<(WlSurface, SmithayPoint<i32, Physical>, f64)> {
        // Compute window geometry once. Previously each frame rebuilt and sorted this list twice,
        // then performed another linear surface-to-window lookup for every item.
        let windows = self.mapped_windows_for_output(output).collect::<Vec<_>>();
        // Ordering here is both render order and the contract mirrored by scene hit testing.
        let mut roots = Vec::new();
        roots.extend(self.layer_roots(output, Layer::Overlay));
        roots.extend(
            windows
                .iter()
                .filter(|(_, _, _, mode)| *mode == WindowMode::Fullscreen)
                .map(|(surface, location, scale, _)| {
                    (surface.wl_surface().clone(), *location, *scale)
                }),
        );
        roots.extend(self.layer_roots(output, Layer::Top));
        roots.extend(
            windows
                .iter()
                .filter(|(_, _, _, mode)| matches!(mode, WindowMode::Floating | WindowMode::Tiled))
                .map(|(surface, location, scale, _)| {
                    (surface.wl_surface().clone(), *location, *scale)
                }),
        );
        roots.extend(self.layer_roots(output, Layer::Bottom));
        roots.extend(self.layer_roots(output, Layer::Background));
        roots
    }

    pub(crate) fn protocol_output(&self, output: OutputId) -> Option<SmithayOutput> {
        self.output_runtime
            .get(&output)
            .map(|runtime| runtime.wayland.clone())
    }

    fn layer_roots(
        &self,
        output: OutputId,
        wanted: Layer,
    ) -> impl Iterator<Item = (WlSurface, SmithayPoint<i32, Physical>, f64)> + '_ {
        let scale = self.output_scale(output);
        self.layers
            .iter()
            .filter(move |mapped| {
                mapped.mapped && mapped.output == output && mapped.layer == wanted
            })
            .filter_map(move |mapped| {
                let (origin, _) = self.layer_geometry(mapped)?;
                Some((
                    mapped.surface.wl_surface().clone(),
                    physical_point(origin, scale),
                    scale,
                ))
            })
    }

    pub(super) fn layer_geometry(&self, mapped: &MappedLayer) -> Option<(Point, Size)> {
        let output = self.desktop.outputs.get(&mapped.output)?;
        let requested = with_states(mapped.surface.wl_surface(), |states| {
            *states
                .cached_state
                .get::<LayerSurfaceCachedState>()
                .current()
        });
        let width = if requested.size.w == 0 {
            (output.output.logical_size.width
                - i64::from(requested.margin.left + requested.margin.right))
            .max(1)
        } else {
            i64::from(requested.size.w)
        };
        let height = if requested.size.h == 0 {
            (output.output.logical_size.height
                - i64::from(requested.margin.top + requested.margin.bottom))
            .max(1)
        } else {
            i64::from(requested.size.h)
        };
        let x = if requested.anchor.contains(Anchor::LEFT) {
            i64::from(requested.margin.left)
        } else if requested.anchor.contains(Anchor::RIGHT) {
            output.output.logical_size.width - width - i64::from(requested.margin.right)
        } else {
            (output.output.logical_size.width - width) / 2
        };
        let y = if requested.anchor.contains(Anchor::TOP) {
            i64::from(requested.margin.top)
        } else if requested.anchor.contains(Anchor::BOTTOM) {
            output.output.logical_size.height - height - i64::from(requested.margin.bottom)
        } else {
            (output.output.logical_size.height - height) / 2
        };
        Some((Point::new(x, y), Size::new(width, height)))
    }

    pub fn update_output_size(&mut self, width: i64, height: i64) {
        let size = Size::new(width, height);
        if self.desktop.outputs[&self.active_output]
            .output
            .logical_size
            != size
        {
            let current = self.desktop.outputs[&self.active_output].clone();
            if let Err(error) = self.configure_output(
                self.active_output,
                size,
                size,
                current.output.native_scale,
                current.output.transform,
            ) {
                tracing::error!(%error, "could not resize nested output");
            }
        }
    }

    pub fn configure_output(
        &mut self,
        output: OutputId,
        physical_size: Size,
        logical_size: Size,
        native_scale: astera_core::Scale120,
        transform: OutputTransform,
    ) -> Result<(), astera_core::DesktopError> {
        self.desktop.configure_output(
            output,
            physical_size,
            logical_size,
            native_scale,
            transform,
        )?;
        let mode = Mode {
            size: (
                saturating_i32(physical_size.width),
                saturating_i32(physical_size.height),
            )
                .into(),
            refresh: 60_000,
        };
        let runtime = self
            .output_runtime
            .get(&output)
            .expect("desktop output has a Wayland runtime");
        runtime.wayland.change_current_state(
            Some(mode),
            Some(output_transform(transform)),
            Some(Scale::Fractional(native_scale.0 as f64 / 120.0)),
            None,
        );
        runtime.wayland.set_preferred(mode);
        self.reflow_outputs();
        self.configure_fullscreen_windows();
        self.configure_layer_surfaces();
        self.refresh_visible_scales();
        Ok(())
    }

    pub(super) fn reflow_outputs(&mut self) {
        let mut x = 0_i64;
        let placements = self
            .desktop
            .outputs
            .iter()
            .map(|(id, output)| {
                let placement = (*id, Point::new(x, 0));
                x = x.saturating_add(output.output.logical_size.width);
                placement
            })
            .collect::<Vec<_>>();
        for (output, location) in placements {
            let runtime = self
                .output_runtime
                .get_mut(&output)
                .expect("desktop output has a Wayland runtime");
            runtime.location = location;
            runtime.wayland.change_current_state(
                None,
                None,
                None,
                Some((saturating_i32(location.x), saturating_i32(location.y)).into()),
            );
        }
    }

    pub(super) fn output_scale(&self, output: OutputId) -> f64 {
        self.desktop.outputs[&output].output.native_scale.0 as f64 / 120.0
    }

    pub(super) fn refresh_visible_scales(&mut self) {
        // A workspace is exclusive to one output, so each entered surface has one authoritative
        // fractional scale. Subsurfaces and popups still need explicit enter/leave propagation.
        let scenes: BTreeMap<_, _> = self
            .output_runtime
            .keys()
            .copied()
            .map(|output| {
                let scale = self.output_scale(output);
                let roots = self
                    .render_roots_for_output(output)
                    .into_iter()
                    .map(|(surface, _, _)| surface)
                    .collect::<Vec<_>>();
                let mut visible = HashSet::new();
                for root in roots {
                    for (popup, _) in PopupManager::popups_for_surface(&root) {
                        extend_surface_tree(&mut visible, popup.wl_surface());
                    }
                    extend_surface_tree(&mut visible, &root);
                }
                (output, (scale, visible))
            })
            .collect();
        for (output, (scale, visible)) in scenes {
            let runtime = self
                .output_runtime
                .get_mut(&output)
                .expect("scene output has a Wayland runtime");
            for surface in &runtime.entered_surfaces {
                if !visible.contains(surface) {
                    runtime.wayland.leave(surface);
                }
            }
            for surface in &visible {
                if !runtime.entered_surfaces.contains(surface) {
                    runtime.wayland.enter(surface);
                }
                with_states(surface, |states| {
                    with_fractional_scale(states, |fractional| {
                        fractional.set_preferred_scale(scale);
                    });
                });
            }
            runtime.entered_surfaces = visible;
        }
    }
}
