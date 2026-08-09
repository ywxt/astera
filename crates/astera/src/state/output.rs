use super::*;

impl Astera {
    pub fn update_primary_scanout_output(
        &mut self,
        output: OutputId,
        roots: &[(WlSurface, SmithayPoint<i32, Physical>, f64)],
        states: &smithay::backend::renderer::element::RenderElementStates,
    ) {
        use smithay::{
            backend::renderer::element::default_primary_scanout_output_compare,
            desktop::utils::{
                surface_primary_scanout_output, update_surface_primary_scanout_output,
            },
        };

        let Some(wayland_output) = self
            .output_runtime
            .get(&output)
            .map(|runtime| runtime.wayland.clone())
        else {
            return;
        };
        let mut presented = HashSet::new();
        let mut update_tree = |root: &WlSurface| {
            with_surface_tree_downward(
                root,
                (),
                |_, _, _| TraversalAction::DoChildren(()),
                |surface, surface_states, _| {
                    update_surface_primary_scanout_output(
                        surface,
                        &wayland_output,
                        surface_states,
                        states,
                        default_primary_scanout_output_compare,
                    );
                    if surface_primary_scanout_output(surface, surface_states).as_ref()
                        == Some(&wayland_output)
                    {
                        presented.insert(surface.clone());
                    }
                },
                |_, _, _| true,
            );
        };
        for (root, _, _) in roots {
            update_tree(root);
            for (popup, _) in PopupManager::popups_for_surface(root) {
                update_tree(popup.wl_surface());
            }
        }
        if let Some(cursor) = self.cursor_surface_for_output(output) {
            update_tree(&cursor);
        }
        if let Some(runtime) = self.output_runtime.get_mut(&output) {
            runtime.presented_surfaces = presented;
        }
        self.refresh_idle_inhibition();
    }

    pub fn enable_dmabuf(&mut self, formats: impl IntoIterator<Item = Format>) {
        if self.dmabuf_enabled {
            return;
        }
        let display = self.display.clone();
        self.dmabuf_state.create_global::<Self>(&display, formats);
        self.dmabuf_enabled = true;
    }

    pub fn validate_dmabuf_imports<R: ImportDma>(&mut self, renderer: &mut R) {
        for (dmabuf, notifier) in self.pending_dmabufs.drain(..) {
            if renderer.import_dmabuf(&dmabuf, None).is_ok() {
                let _ = notifier.successful::<Self>();
            } else {
                notifier.failed();
            }
        }
    }

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
        // Locking immediately hides every desktop surface. Before the locker attaches a buffer,
        // the renderer clear colour is the fail-closed frame for this output.
        if self.session_is_locked() {
            return self
                .lock_surface_for_output(output)
                .map(|surface| {
                    vec![(
                        surface.wl_surface().clone(),
                        (0, 0).into(),
                        self.output_scale(output),
                    )]
                })
                .unwrap_or_default();
        }
        // Compute window geometry once. Previously each frame rebuilt and sorted this list twice,
        // then performed another linear surface-to-window lookup for every item.
        let windows = self.mapped_windows_for_output(output).collect::<Vec<_>>();
        let has_fullscreen = windows
            .iter()
            .any(|(_, _, _, mode)| *mode == WindowMode::Fullscreen);
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
                .filter(|(_, _, _, mode)| {
                    !has_fullscreen
                        && matches!(
                            mode,
                            WindowMode::Maximized | WindowMode::Floating | WindowMode::Tiled
                        )
                })
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
        let runtime = self.output_runtime.get(&mapped.output)?;
        let geometry = layer_map_for_output(&runtime.wayland).layer_geometry(&mapped.surface)?;
        Some((
            Point::new(i64::from(geometry.loc.x), i64::from(geometry.loc.y)),
            Size::new(i64::from(geometry.size.w), i64::from(geometry.size.h)),
        ))
    }

    pub(super) fn usable_rect(&self, output: OutputId) -> Option<astera_core::Rect> {
        let runtime = self.output_runtime.get(&output)?;
        let zone = layer_map_for_output(&runtime.wayland).non_exclusive_zone();
        Some(astera_core::Rect::new(
            i64::from(zone.loc.x),
            i64::from(zone.loc.y),
            i64::from(zone.size.w),
            i64::from(zone.size.h),
        ))
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
        layer_map_for_output(&runtime.wayland).arrange();
        self.reflow_outputs();
        self.configure_fullscreen_windows();
        self.configure_layer_surfaces();
        self.configure_lock_surface(output);
        self.refresh_visible_scales();
        self.mark_public_dirty();
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
                if let Some(cursor) = self.cursor_surface_for_output(output) {
                    extend_surface_tree(&mut visible, &cursor);
                }
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
            let departed = runtime
                .presented_surfaces
                .difference(&visible)
                .cloned()
                .collect::<Vec<_>>();
            let empty = smithay::backend::renderer::element::RenderElementStates::default();
            for surface in departed {
                with_states(&surface, |states| {
                    smithay::desktop::utils::update_surface_primary_scanout_output(
                        &surface,
                        &runtime.wayland,
                        states,
                        &empty,
                        smithay::backend::renderer::element::default_primary_scanout_output_compare,
                    );
                });
            }
            runtime.entered_surfaces = visible;
            runtime
                .presented_surfaces
                .retain(|surface| runtime.entered_surfaces.contains(surface));
        }
        self.refresh_idle_inhibition();
    }
}
