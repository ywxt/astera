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
        if let Some((icon, _, _)) = self.dnd_icon_render_source(output) {
            update_tree(&icon);
        }
        if let Some(runtime) = self.output_runtime.get_mut(&output) {
            runtime.presented_surfaces = presented;
        }
        self.refresh_idle_inhibition();
    }

    pub fn enable_dmabuf(
        &mut self,
        main_device: Option<u64>,
        formats: impl IntoIterator<Item = Format>,
    ) {
        let formats = formats.into_iter().collect::<Vec<_>>();
        if let Some(main_device) = main_device {
            self.dmabuf_devices.insert(main_device, formats.clone());
        }
        if self.dmabuf_enabled {
            if self.dmabuf_default_device.is_none()
                && let Some(main_device) = main_device
            {
                self.rebase_dmabuf_feedback(main_device);
            }
            return;
        }
        let display = self.display.clone();
        if let Some(main_device) = main_device {
            match smithay::wayland::dmabuf::DmabufFeedbackBuilder::new(
                main_device,
                formats.iter().copied(),
            )
            .build()
            {
                Ok(feedback) => {
                    self.dmabuf_global = Some(
                        self.dmabuf_state
                            .create_global_with_default_feedback::<Self>(&display, &feedback),
                    );
                    self.dmabuf_default_device = Some(main_device);
                    self.dmabuf_default_formats = formats.clone();
                }
                Err(error) => {
                    tracing::warn!(%error, "could not build linux-dmabuf v4 feedback; advertising v3");
                    self.dmabuf_global = Some(
                        self.dmabuf_state
                            .create_global::<Self>(&display, formats.iter().copied()),
                    );
                }
            }
        } else {
            tracing::warn!("renderer has no DRM render node; advertising linux-dmabuf v3");
            self.dmabuf_global = Some(
                self.dmabuf_state
                    .create_global::<Self>(&display, formats.iter().copied()),
            );
        }
        self.dmabuf_enabled = true;
    }

    pub fn register_output_dmabuf_feedback(
        &mut self,
        output: OutputId,
        target_device: u64,
        formats: impl IntoIterator<Item = Format>,
    ) {
        let formats = formats.into_iter().collect::<Vec<_>>();
        self.dmabuf_devices
            .entry(target_device)
            .or_insert_with(|| formats.clone());
        self.dmabuf_output_devices.insert(output, target_device);
        self.rebuild_output_dmabuf_feedback(output);
        self.refresh_requested_dmabuf_feedbacks();
        self.refresh_visible_scales();
    }

    pub fn unregister_dmabuf_device(&mut self, device: u64) {
        self.dmabuf_devices.remove(&device);
        self.dmabuf_output_devices
            .retain(|_, target| *target != device);
        self.dmabuf_output_feedback
            .retain(|output, _| self.dmabuf_output_devices.contains_key(output));
        if self.dmabuf_default_device == Some(device) {
            if let Some(replacement) = self.dmabuf_devices.keys().next().copied() {
                self.rebase_dmabuf_feedback(replacement);
            } else {
                self.dmabuf_default_device = None;
                self.dmabuf_default_formats.clear();
                self.dmabuf_output_feedback.clear();
            }
        }
        self.refresh_requested_dmabuf_feedbacks();
        self.refresh_visible_scales();
    }

    fn rebase_dmabuf_feedback(&mut self, main_device: u64) {
        let Some(formats) = self.dmabuf_devices.get(&main_device).cloned() else {
            return;
        };
        let Ok(feedback) = smithay::wayland::dmabuf::DmabufFeedbackBuilder::new(
            main_device,
            formats.iter().copied(),
        )
        .build() else {
            tracing::warn!(main_device, "could not rebuild default dmabuf feedback");
            return;
        };
        let Some(global) = self.dmabuf_global else {
            return;
        };
        self.dmabuf_state.set_default_feedback(&global, &feedback);
        self.dmabuf_default_device = Some(main_device);
        self.dmabuf_default_formats = formats;
        self.dmabuf_output_feedback.clear();
        let outputs = self
            .dmabuf_output_devices
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for output in outputs {
            self.rebuild_output_dmabuf_feedback(output);
        }
        self.refresh_requested_dmabuf_feedbacks();
        self.refresh_visible_scales();
    }

    pub(super) fn refresh_requested_dmabuf_feedbacks(&mut self) {
        self.dmabuf_feedback_surfaces
            .retain(smithay::utils::IsAlive::alive);
        let updates = self
            .dmabuf_feedback_surfaces
            .iter()
            .filter_map(|surface| {
                self.dmabuf_feedback_for_surface(surface)
                    .map(|feedback| (surface.clone(), feedback))
            })
            .collect::<Vec<_>>();
        for (surface, feedback) in updates {
            with_states(&surface, |states| {
                if let Some(surface_feedback) = SurfaceDmabufFeedbackState::from_states(states) {
                    surface_feedback.set_feedback(&feedback);
                }
            });
        }
    }

    fn rebuild_output_dmabuf_feedback(&mut self, output: OutputId) {
        let Some(main_device) = self.dmabuf_default_device else {
            return;
        };
        let Some(target_device) = self.dmabuf_output_devices.get(&output).copied() else {
            return;
        };
        let Some(formats) = self.dmabuf_devices.get(&target_device).cloned() else {
            return;
        };
        let mut builder = smithay::wayland::dmabuf::DmabufFeedbackBuilder::new(
            main_device,
            self.dmabuf_default_formats.iter().copied(),
        );
        if target_device != main_device {
            builder = builder.add_preference_tranche(target_device, None, formats.iter().copied());
        }
        match builder.build() {
            Ok(feedback) => {
                self.dmabuf_output_feedback.insert(output, feedback);
            }
            Err(error) => {
                tracing::warn!(?output, %error, "could not build per-output dmabuf feedback");
            }
        }
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

    pub fn has_pending_dmabuf_imports(&self) -> bool {
        !self.pending_dmabufs.is_empty()
    }

    pub fn fail_pending_dmabuf_imports(&mut self) {
        for (_, notifier) in self.pending_dmabufs.drain(..) {
            notifier.failed();
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
        roots.extend(self.input_method_roots(output));
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
        self.reconstrain_reactive_popups();
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
                if let Some((icon, _, _)) = self.dnd_icon_render_source(output) {
                    extend_surface_tree(&mut visible, &icon);
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
            let dmabuf_feedback = self.dmabuf_output_feedback.get(&output).cloned();
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
                    if let Some(feedback) = dmabuf_feedback.as_ref()
                        && let Some(surface_feedback) =
                            SurfaceDmabufFeedbackState::from_states(states)
                    {
                        surface_feedback.set_feedback(feedback);
                    }
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
