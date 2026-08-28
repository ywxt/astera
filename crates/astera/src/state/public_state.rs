use astera_core::{
    FullscreenRestorePlacement, MinimizedRestorePlacement, RestorePlacement, WindowMode,
};
use astera_ipc::wire::v1::{
    Anchor as PublicAnchor, BaseRestore as PublicBaseRestore, CameraSnapshot, ConfigSnapshot,
    DesktopSnapshot, ExclusiveContribution, FullscreenRestore as PublicFullscreenRestore,
    KeyboardInteractivity as PublicKeyboardInteractivity, Layer as PublicLayer, LayerSnapshot,
    OutputSnapshot, WindowMetadata, WindowPlacement, WindowSnapshot, WorkspaceSnapshot,
};
use smithay::wayland::{
    compositor::with_states,
    shell::{
        wlr_layer::{Anchor, ExclusiveZone, KeyboardInteractivity, Layer, LayerSurfaceCachedState},
        xdg::XdgToplevelSurfaceData,
    },
};

use super::Astera;

impl Astera {
    /// Build the canonical public view from compositor state, including protocol-only metadata.
    /// Every repeated collection is sorted by stable identity before it crosses the IPC boundary.
    pub fn public_snapshot(&self) -> DesktopSnapshot {
        let mut outputs = self
            .desktop
            .outputs
            .values()
            .map(|set| {
                let output = &set.output;
                let viewport = astera_core::Rect::new(
                    0,
                    0,
                    output.logical_size.width,
                    output.logical_size.height,
                );
                OutputSnapshot {
                    id: output.id.into(),
                    stable_key: output.stable_key.clone(),
                    active_workspace: set.active_workspace().expect("normalized output").id.into(),
                    workspaces: set
                        .workspaces
                        .iter()
                        .map(|workspace| workspace.id.into())
                        .collect(),
                    physical_size: output.physical_size.into(),
                    logical_size: output.logical_size.into(),
                    native_scale: output.native_scale.into(),
                    transform: output.transform.into(),
                    viewport: viewport.into(),
                    usable_area: self.usable_rect(output.id).unwrap_or(viewport).into(),
                }
            })
            .collect::<Vec<_>>();
        outputs.sort_by_key(|output| output.id);

        let mut workspaces = Vec::new();
        let mut cameras = Vec::new();
        let mut windows = Vec::new();
        for workspace in self.desktop.workspaces() {
            let location = self
                .desktop
                .workspace_location(workspace.id)
                .expect("workspace came from desktop");
            workspaces.push(WorkspaceSnapshot {
                id: workspace.id.into(),
                name: workspace.name.clone(),
                original_output: workspace.original_output.clone(),
                output: location.output.map(Into::into),
                local_index: location.output.map(|_| {
                    u32::try_from(location.index + 1).expect("workspace index fits public schema")
                }),
                active_window: workspace.focused_window.map(Into::into),
                tiled_count: workspace.tiled.len() as u64,
                floating_count: workspace.floating.len() as u64,
                fullscreen: workspace.fullscreen.as_ref().map(|full| full.window.into()),
            });
            cameras.push(CameraSnapshot {
                workspace: workspace.id.into(),
                center: workspace.camera.center.into(),
                policy: workspace.camera.policy.into(),
            });
            for tiled in workspace.tiled.values() {
                windows.push(self.window_snapshot(
                    workspace.id,
                    tiled.id,
                    WindowMode::Tiled,
                    WindowPlacement::Tiled {
                        world_geometry: tiled.geometry.into(),
                    },
                    location.output,
                ));
            }
            for floating in workspace.floating.values() {
                windows.push(self.window_snapshot(
                    workspace.id,
                    floating.window,
                    WindowMode::Floating,
                    WindowPlacement::Floating {
                        viewport_geometry: floating.viewport.rect.into(),
                    },
                    location.output,
                ));
            }
            if let Some(maximized) = &workspace.maximized {
                windows.push(self.window_snapshot(
                    workspace.id,
                    maximized.window,
                    WindowMode::Maximized,
                    WindowPlacement::Maximized {
                        restore: public_restore(&maximized.restore),
                    },
                    location.output,
                ));
            }
            if let Some(fullscreen) = &workspace.fullscreen {
                windows.push(self.window_snapshot(
                    workspace.id,
                    fullscreen.window,
                    WindowMode::Fullscreen,
                    WindowPlacement::Fullscreen {
                        restore: fullscreen_restore(&fullscreen.restore),
                    },
                    location.output,
                ));
            }
            for minimized in workspace.minimized.values() {
                let (mode, placement) = public_minimized(&minimized.restore);
                windows.push(self.window_snapshot(
                    workspace.id,
                    minimized.window,
                    mode,
                    placement,
                    location.output,
                ));
            }
        }
        workspaces.sort_by_key(|workspace| workspace.id);
        cameras.sort_by_key(|camera| camera.workspace);
        windows.sort_by_key(|window| window.id);

        let mut layers = self
            .layers
            .iter()
            .filter(|mapped| mapped.mapped)
            .filter_map(|mapped| {
                let (origin, size) = self.layer_geometry(mapped)?;
                let state = with_states(mapped.surface.wl_surface(), |states| {
                    *states
                        .cached_state
                        .get::<LayerSurfaceCachedState>()
                        .current()
                });
                Some(LayerSnapshot {
                    id: mapped.id,
                    output: mapped.output.into(),
                    namespace: mapped.surface.namespace().to_owned(),
                    layer: public_layer(mapped.layer),
                    anchor: public_anchor(state.anchor),
                    exclusive_zone: match state.exclusive_zone {
                        ExclusiveZone::DontCare => -1,
                        ExclusiveZone::Neutral => 0,
                        ExclusiveZone::Exclusive(amount) => {
                            i32::try_from(amount).unwrap_or(i32::MAX)
                        }
                    },
                    exclusive_contribution: exclusive_contribution(&state),
                    keyboard_interactivity: public_keyboard(state.keyboard_interactivity),
                    geometry: astera_core::Rect { origin, size }.into(),
                })
            })
            .collect::<Vec<_>>();
        layers.sort_by_key(|layer| layer.id);

        DesktopSnapshot {
            active_output: self
                .desktop
                .outputs
                .contains_key(&self.active_output)
                .then_some(self.active_output.into()),
            primary_output: self.desktop.primary_output.map(Into::into),
            focused_window: self
                .desktop
                .outputs
                .get(&self.active_output)
                .and_then(|set| set.active_workspace())
                .and_then(|workspace| workspace.focused_window)
                .map(Into::into),
            outputs,
            layers,
            workspaces,
            cameras,
            windows,
            config: ConfigSnapshot {
                source: self.config_source.clone(),
                generation: self.config_generation,
                failed: self.config_failed,
                error: self.config_error.clone(),
            },
        }
    }

    fn window_snapshot(
        &self,
        workspace: astera_core::WorkspaceId,
        window: astera_core::WindowId,
        mode: WindowMode,
        placement: WindowPlacement,
        output: Option<astera_core::OutputId>,
    ) -> WindowSnapshot {
        let metadata = self
            .windows
            .iter()
            .find(|mapped| mapped.id == window && mapped.mapped)
            .map(|mapped| {
                with_states(mapped.surface.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .map(|data| {
                            let attributes = data.lock().unwrap();
                            WindowMetadata {
                                title: attributes.title.clone(),
                                app_id: attributes.app_id.clone(),
                                tag: mapped.tag.clone(),
                                description: mapped.description.clone(),
                                icon_name: mapped.icon_name.clone(),
                            }
                        })
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();
        let mapped = self
            .windows
            .iter()
            .any(|mapped| mapped.id == window && mapped.mapped);
        let visible_geometry = mapped.then_some(output).flatten().and_then(|output| {
            self.desktop
                .workspace_for_output(output)
                .is_some_and(|visible| visible.id == workspace)
                .then(|| self.visual_geometry_for_output(output, window))
                .flatten()
                .map(|(origin, size, _, _)| astera_core::Rect { origin, size }.into())
        });
        WindowSnapshot {
            id: window.into(),
            workspace: workspace.into(),
            mode: mode.into(),
            metadata,
            placement,
            visible_geometry,
            urgent: self
                .windows
                .iter()
                .find(|mapped| mapped.id == window)
                .is_some_and(|mapped| mapped.urgent),
        }
    }

    /// Mark an externally observable mutation for canonical snapshot diffing at tick end.
    pub(super) fn mark_public_dirty(&mut self) {
        self.public_dirty = true;
        self.mark_render_dirty();
    }

    /// Publish a tick-end snapshot only when externally observable state may have changed.
    ///
    /// Commands may still force the compositor to reach this boundary before replying; a clean
    /// boundary neither rebuilds the snapshot nor manufactures an event.
    pub fn publish_public_state(&mut self) -> &[astera_ipc::wire::v1::EventEnvelope] {
        if !self.public_dirty {
            return self.event_hub.clean_tick();
        }
        self.sync_workspace_protocol();
        self.public_dirty = false;
        #[cfg(test)]
        {
            self.public_snapshot_builds += 1;
        }
        let snapshot = self.public_snapshot();
        self.event_hub.publish(snapshot)
    }

    pub fn public_sequence(&self) -> u64 {
        self.event_hub.sequence()
    }

    #[allow(dead_code)] // The event-stream broadcaster consumes this hook in the next batch.
    pub fn take_public_sequence_overflow(&mut self) -> bool {
        self.event_hub.take_sequence_overflow()
    }

    pub(super) fn record_config_loaded(&mut self, error: Option<String>) {
        self.config_generation = self
            .config_generation
            .checked_add(1)
            .expect("config generation exhausted");
        self.config_failed = error.is_some();
        self.config_error = error.clone();
        self.event_hub
            .config_loaded(self.config_generation, self.config_failed, error);
        self.mark_public_dirty();
    }
}

fn public_restore(restore: &RestorePlacement) -> PublicBaseRestore {
    match restore {
        RestorePlacement::Tiled { world_rect } => PublicBaseRestore::Tiled {
            world_geometry: (*world_rect).into(),
        },
        RestorePlacement::Floating { viewport } => PublicBaseRestore::Floating {
            viewport_geometry: viewport.rect.into(),
        },
    }
}

fn fullscreen_restore(restore: &FullscreenRestorePlacement) -> PublicFullscreenRestore {
    match restore {
        FullscreenRestorePlacement::Tiled { world_rect } => PublicFullscreenRestore::Tiled {
            world_geometry: (*world_rect).into(),
        },
        FullscreenRestorePlacement::Floating { viewport } => PublicFullscreenRestore::Floating {
            viewport_geometry: viewport.rect.into(),
        },
        FullscreenRestorePlacement::Maximized { restore } => PublicFullscreenRestore::Maximized {
            restore: public_restore(restore),
        },
    }
}

fn public_minimized(restore: &MinimizedRestorePlacement) -> (WindowMode, WindowPlacement) {
    match restore {
        MinimizedRestorePlacement::Tiled { world_rect } => (
            WindowMode::Tiled,
            WindowPlacement::Tiled {
                world_geometry: (*world_rect).into(),
            },
        ),
        MinimizedRestorePlacement::Floating { viewport } => (
            WindowMode::Floating,
            WindowPlacement::Floating {
                viewport_geometry: viewport.rect.into(),
            },
        ),
        MinimizedRestorePlacement::Maximized { restore } => (
            WindowMode::Maximized,
            WindowPlacement::Maximized {
                restore: public_restore(restore),
            },
        ),
        MinimizedRestorePlacement::Fullscreen { restore } => (
            WindowMode::Fullscreen,
            WindowPlacement::Fullscreen {
                restore: fullscreen_restore(restore),
            },
        ),
    }
}

fn public_layer(layer: Layer) -> PublicLayer {
    match layer {
        Layer::Background => PublicLayer::Background,
        Layer::Bottom => PublicLayer::Bottom,
        Layer::Top => PublicLayer::Top,
        Layer::Overlay => PublicLayer::Overlay,
    }
}

fn public_anchor(anchor: Anchor) -> PublicAnchor {
    PublicAnchor {
        top: anchor.contains(Anchor::TOP),
        bottom: anchor.contains(Anchor::BOTTOM),
        left: anchor.contains(Anchor::LEFT),
        right: anchor.contains(Anchor::RIGHT),
    }
}

fn public_keyboard(interactivity: KeyboardInteractivity) -> PublicKeyboardInteractivity {
    match interactivity {
        KeyboardInteractivity::None => PublicKeyboardInteractivity::None,
        KeyboardInteractivity::Exclusive => PublicKeyboardInteractivity::Exclusive,
        KeyboardInteractivity::OnDemand => PublicKeyboardInteractivity::OnDemand,
    }
}

fn exclusive_contribution(state: &LayerSurfaceCachedState) -> ExclusiveContribution {
    let ExclusiveZone::Exclusive(amount) = state.exclusive_zone else {
        return ExclusiveContribution::default();
    };
    let amount = i64::from(amount);
    let anchor = state.anchor;
    let mut contribution = ExclusiveContribution::default();
    // Keep this branch order in sync with Smithay's LayerMap::arrange(): surfaces spanning both
    // vertical edges reserve horizontal space, and surfaces spanning both horizontal edges reserve
    // vertical space. Perpendicular margins also contribute to the resulting usable-area delta.
    if anchor.contains(Anchor::TOP) && anchor.contains(Anchor::BOTTOM) {
        if anchor.contains(Anchor::LEFT) {
            contribution.left = amount + i64::from(state.margin.left);
        } else if anchor.contains(Anchor::RIGHT) {
            contribution.right = amount + i64::from(state.margin.right);
        } else {
            contribution.right = amount;
        }
        if anchor.contains(Anchor::LEFT) && anchor.contains(Anchor::RIGHT) {
            contribution.right = i64::from(state.margin.right);
        }
    } else if anchor.contains(Anchor::LEFT) && anchor.contains(Anchor::RIGHT) {
        if anchor.contains(Anchor::TOP) {
            contribution.top = amount + i64::from(state.margin.top);
        } else if anchor.contains(Anchor::BOTTOM) {
            contribution.bottom = amount + i64::from(state.margin.bottom);
        } else {
            contribution.bottom = amount;
        }
        if anchor.contains(Anchor::TOP) && anchor.contains(Anchor::BOTTOM) {
            contribution.bottom = i64::from(state.margin.bottom);
        }
    } else if anchor.contains(Anchor::LEFT) && !anchor.contains(Anchor::RIGHT) {
        contribution.left = amount + i64::from(state.margin.left);
    } else if anchor.contains(Anchor::RIGHT) && !anchor.contains(Anchor::LEFT) {
        contribution.right = amount + i64::from(state.margin.right);
    } else if anchor.contains(Anchor::TOP) && !anchor.contains(Anchor::BOTTOM) {
        contribution.top = amount + i64::from(state.margin.top);
    } else if anchor.contains(Anchor::BOTTOM) && !anchor.contains(Anchor::TOP) {
        contribution.bottom = amount + i64::from(state.margin.bottom);
    }
    contribution
}

#[cfg(test)]
mod tests {
    use smithay::reexports::wayland_server::Display;

    use super::*;

    #[test]
    fn snapshot_contains_runtime_output_workspace_and_config_status() {
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        state.config_source = Some("/tmp/astera.kdl".into());
        let snapshot = state.public_snapshot();
        assert_eq!(snapshot.outputs.len(), 1);
        assert_eq!(
            snapshot.outputs[0].viewport,
            astera_ipc::wire::v1::Rect::new(0, 0, 1280, 720)
        );
        assert_eq!(
            snapshot.outputs[0].usable_area,
            snapshot.outputs[0].viewport
        );
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.cameras[0].workspace, snapshot.workspaces[0].id);
        assert_eq!(snapshot.config.source.as_deref(), Some("/tmp/astera.kdl"));
        assert_eq!(snapshot.config.generation, 0);
        assert!(!snapshot.config.failed);
        assert_eq!(snapshot.config.error, None);
    }

    #[test]
    fn snapshot_exposes_maximized_placement_but_not_unmapped_geometry() {
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        let workspace = state
            .desktop
            .active_workspace_id(astera_core::OutputId(0))
            .unwrap();
        let window = astera_core::WindowId(42);
        state
            .desktop
            .apply_window(
                workspace,
                astera_core::WindowTransaction::InsertTiled {
                    id: window,
                    size: astera_core::Size::new(640, 480),
                    anchor: astera_core::Point::ORIGIN,
                    seed_direction: astera_core::Direction::RIGHT,
                },
            )
            .unwrap();
        state
            .desktop
            .apply_window(
                workspace,
                astera_core::WindowTransaction::SetMode {
                    id: window,
                    mode: WindowMode::Maximized,
                    viewport_size: astera_core::Size::new(1280, 720),
                },
            )
            .unwrap();
        let snapshot = state.public_snapshot();
        let window = snapshot
            .windows
            .iter()
            .find(|candidate| candidate.id.0 == 42)
            .unwrap();
        assert!(matches!(
            window.placement,
            WindowPlacement::Maximized { .. }
        ));
        assert_eq!(
            window.visible_geometry, None,
            "persistent placement does not imply that a surface is currently rendered"
        );
    }

    #[test]
    fn snapshot_keeps_minimized_windows_addressable_but_invisible() {
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        let workspace = state
            .desktop
            .active_workspace_id(astera_core::OutputId(0))
            .unwrap();
        let window = astera_core::WindowId(77);
        state
            .desktop
            .apply_window(
                workspace,
                astera_core::WindowTransaction::InsertTiled {
                    id: window,
                    size: astera_core::Size::new(640, 480),
                    anchor: astera_core::Point::ORIGIN,
                    seed_direction: astera_core::Direction::RIGHT,
                },
            )
            .unwrap();
        state
            .desktop
            .apply_window(
                workspace,
                astera_core::WindowTransaction::SetMode {
                    id: window,
                    mode: WindowMode::Minimized,
                    viewport_size: astera_core::Size::new(1280, 720),
                },
            )
            .unwrap();

        let snapshot = state.public_snapshot();
        let window = snapshot
            .windows
            .iter()
            .find(|candidate| candidate.id.0 == 77)
            .unwrap();
        assert_eq!(window.mode, astera_ipc::wire::v1::WindowMode::Tiled);
        assert!(window.visible_geometry.is_none());
        assert!(matches!(window.placement, WindowPlacement::Tiled { .. }));
    }

    #[test]
    fn config_attempt_updates_authoritative_status_and_emits_completion() {
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        state.publish_public_state();
        state.record_config_loaded(Some("invalid binding".into()));

        let snapshot = state.public_snapshot();
        assert_eq!(snapshot.config.generation, 1);
        assert!(snapshot.config.failed);
        assert_eq!(snapshot.config.error.as_deref(), Some("invalid binding"));
        let events = state.publish_public_state();
        assert!(matches!(
            events.last().map(|event| &event.event),
            Some(astera_ipc::wire::v1::Event::ConfigLoaded {
                generation: 1,
                failed: true,
                error: Some(error),
            }) if error == "invalid binding"
        ));
    }

    #[test]
    fn clean_ticks_do_not_rebuild_the_public_snapshot() {
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());

        state.publish_public_state();
        assert_eq!(state.public_snapshot_builds, 1);
        assert!(state.publish_public_state().is_empty());
        assert!(state.publish_public_state().is_empty());
        assert_eq!(state.public_snapshot_builds, 1);
    }

    #[test]
    fn commands_output_changes_and_config_events_mark_public_state_dirty() {
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        state.publish_public_state();

        let workspace = state.public_snapshot().outputs[0].active_workspace;
        assert!(matches!(
            state.execute_command(astera_ipc::Command::PanCamera {
                workspace,
                dx: 1,
                dy: 0,
            }),
            astera_ipc::Response::Success(_)
        ));
        assert!(!state.publish_public_state().is_empty());
        assert_eq!(state.public_snapshot_builds, 2);

        state.update_output_size(1024, 768);
        assert!(!state.publish_public_state().is_empty());
        assert_eq!(state.public_snapshot_builds, 3);

        state.record_config_loaded(None);
        assert!(state.publish_public_state().iter().any(|event| matches!(
            event.event,
            astera_ipc::wire::v1::Event::ConfigLoaded { .. }
        )));
        assert_eq!(state.public_snapshot_builds, 4);
    }
}
