use super::*;

impl BufferHandler for Astera {
    fn buffer_destroyed(&mut self, buffer: &wl_buffer::WlBuffer) {
        self.icon_buffer_destroyed(buffer);
    }
}

impl IdleInhibitHandler for Astera {
    fn inhibit(&mut self, surface: WlSurface) {
        let count = self.idle_inhibitors.entry(surface).or_default();
        *count = count.saturating_add(1);
        self.refresh_idle_inhibition();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        if let Some(count) = self.idle_inhibitors.get_mut(&surface) {
            *count -= 1;
            if *count == 0 {
                self.idle_inhibitors.remove(&surface);
            }
        }
        self.refresh_idle_inhibition();
    }
}

impl XdgDecorationHandler for Astera {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.configure_client_side_decoration(&toplevel);
    }

    fn request_mode(
        &mut self,
        toplevel: ToplevelSurface,
        _mode: smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    ) {
        self.configure_client_side_decoration(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.configure_client_side_decoration(&toplevel);
    }
}

impl XdgDialogHandler for Astera {
    fn modal_changed(&mut self, _toplevel: ToplevelSurface, _is_modal: bool) {
        self.mark_public_dirty();
    }
}

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_wm_dialog_v1::XdgWmDialogV1,
        (),
    > for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        manager: &smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_wm_dialog_v1::XdgWmDialogV1,
        request: smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_wm_dialog_v1::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_wm_dialog_v1::{
            Error, Request,
        };

        match request {
            Request::GetXdgDialog { id, toplevel } => {
                let Some(handle) = state.xdg_shell_state.get_toplevel(&toplevel) else {
                    return;
                };
                if state.xdg_dialog_toplevels.contains(&toplevel) {
                    data_init.init(id, handle);
                    manager.post_error(
                        Error::AlreadyUsed,
                        "toplevel dialog is already constructed",
                    );
                    return;
                }
                <XdgDialogState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
                    state,
                    client,
                    manager,
                    Request::GetXdgDialog {
                        id,
                        toplevel: toplevel.clone(),
                    },
                    data,
                    display,
                    data_init,
                );
                state.xdg_dialog_toplevels.insert(toplevel);
            }
            request => {
                <XdgDialogState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
                    state, client, manager, request, data, display, data_init,
                );
            }
        }
    }
}

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_dialog_v1::XdgDialogV1,
        ToplevelSurface,
    > for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        dialog: &smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_dialog_v1::XdgDialogV1,
        request: smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_dialog_v1::Request,
        toplevel: &ToplevelSurface,
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_dialog_v1::Request;
        if matches!(request, Request::Destroy) {
            state.xdg_dialog_toplevels.remove(toplevel.xdg_toplevel());
        }
        <XdgDialogState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, dialog, request, toplevel, display, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _dialog: &smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_dialog_v1::XdgDialogV1,
        toplevel: &ToplevelSurface,
    ) {
        state.xdg_dialog_toplevels.remove(toplevel.xdg_toplevel());
    }
}

impl XdgSystemBellHandler for Astera {
    fn ring(&mut self, surface: Option<WlSurface>) {
        const FLASH_DURATION: std::time::Duration = std::time::Duration::from_millis(150);

        let target = surface
            .as_ref()
            .and_then(|surface| {
                self.windows
                    .iter()
                    .find(|window| {
                        window.mapped && surface_tree_contains(window.surface.wl_surface(), surface)
                    })
                    .map(|window| window.id)
            })
            .or_else(|| {
                self.desktop
                    .active_workspace_id(self.active_output)
                    .and_then(|workspace| self.desktop.workspace(workspace).ok()?.focused_window)
            });
        if let Some(target) = target
            && let Some(window) = self.windows.iter_mut().find(|window| window.id == target)
        {
            window.urgent = true;
            self.mark_public_dirty();
        }
        self.bell_flash_until = Some(self.clock.now() + FLASH_DURATION);
        self.mark_render_dirty();
    }
}

impl XdgActivationHandler for Astera {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, _data: XdgActivationTokenData) -> bool {
        const MAX_PENDING_TOKENS: usize = 1024;
        const TOKEN_LIFETIME: std::time::Duration = std::time::Duration::from_secs(10);

        // A client may commit tokens and never activate them. Prune before admitting new entries
        // and cap the remainder so this protocol cannot become an unbounded memory sink.
        let now = self.clock.now();
        self.xdg_activation_state.retain_tokens(|_, data| {
            now.saturating_duration_since(data.timestamp) <= TOKEN_LIFETIME
        });
        self.xdg_activation_state.tokens().count() < MAX_PENDING_TOKENS
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Tokens are one-shot even when rejected, preventing replay after policy conditions change.
        self.xdg_activation_state.remove_token(&token);
        let Some(index) = self
            .windows
            .iter()
            .position(|window| window.mapped && window.surface.wl_surface() == &surface)
        else {
            return;
        };

        let seat = self.seat.clone();
        let clock = self.clock.clone();
        let authorized = self
            .activation_tracker
            .authorize(&token_data, &seat, clock.as_ref());
        let active_fullscreen_surface = self
            .desktop
            .workspace_for_output(self.active_output)
            .and_then(|workspace| workspace.fullscreen.as_ref())
            .and_then(|full| self.windows.iter().find(|window| window.id == full.window))
            .map(|window| window.surface.wl_surface());
        let fullscreen_blocks = active_fullscreen_surface
            .is_some_and(|fullscreen| !same_application(fullscreen, &surface));

        if !authorized || fullscreen_blocks {
            self.windows[index].urgent = true;
            tracing::debug!(window = ?self.windows[index].id, authorized, fullscreen_blocks, "activation denied; window marked urgent");
            self.mark_public_dirty();
            return;
        }

        let window = self.windows[index].id;
        let was_minimized = self
            .desktop
            .find_window(window)
            .ok()
            .and_then(|workspace| self.desktop.workspace(workspace).ok())
            .is_some_and(|workspace| workspace.window_mode(window) == Some(WindowMode::Minimized));
        if let Ok(workspace) = self.desktop.focus_window(window) {
            if was_minimized
                && let Some(mode) = self
                    .desktop
                    .workspace(workspace)
                    .ok()
                    .and_then(|workspace| workspace.window_mode(window))
            {
                self.configure_window_mode(window, mode);
            }
            if let Ok(location) = self.desktop.workspace_location(workspace)
                && let Some(output) = location.output
            {
                self.active_output = output;
            }
            self.windows[index].urgent = false;
            self.sync_keyboard_focus();
            self.mark_public_dirty();
            tracing::debug!(?window, ?workspace, "activation accepted");
        }
    }
}

fn same_application(first: &WlSurface, second: &WlSurface) -> bool {
    fn app_id(surface: &WlSurface) -> Option<String> {
        with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok()?.app_id.clone())
        })
    }

    match (app_id(first), app_id(second)) {
        (Some(first), Some(second)) => first == second,
        // app_id is client-provided and optional. Client identity is the conservative fallback:
        // separate unidentified connections are treated as different applications.
        _ => first.client().map(|client| client.id()) == second.client().map(|client| client.id()),
    }
}

fn xdg_toplevel_metadata(surface: &ToplevelSurface) -> (String, String) {
    with_states(surface.wl_surface(), |states| {
        let Some(attributes) = states.data_map.get::<XdgToplevelSurfaceData>() else {
            return (String::new(), String::new());
        };
        let Ok(attributes) = attributes.lock() else {
            return (String::new(), String::new());
        };
        (
            attributes.title.clone().unwrap_or_default(),
            attributes.app_id.clone().unwrap_or_default(),
        )
    })
}

impl Astera {
    fn configure_client_side_decoration(&self, toplevel: &ToplevelSurface) {
        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;

        // Astera currently has no SSD renderer, so never advertise a mode it cannot draw.
        toplevel.with_pending_state(|state| state.decoration_mode = Some(Mode::ClientSide));
        toplevel.send_pending_configure();
    }
}

impl DmabufHandler for Astera {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // Renderer ownership lives in the backend, so validation is deferred until its next tick.
        if !dmabuf_import_queue_has_capacity(self.pending_dmabufs.len()) {
            tracing::warn!(
                pending = self.pending_dmabufs.len(),
                "rejecting dmabuf import because the validation queue is full"
            );
            notifier.failed();
            return;
        }
        self.pending_dmabufs.push((dmabuf, notifier));
    }

    fn new_surface_feedback(
        &mut self,
        surface: &WlSurface,
        _global: &DmabufGlobal,
    ) -> Option<DmabufFeedback> {
        self.dmabuf_feedback_surfaces.insert(surface.clone());
        self.dmabuf_feedback_for_surface(surface)
    }
}

impl smithay::wayland::drm_syncobj::DrmSyncobjHandler for Astera {
    fn drm_syncobj_state(&mut self) -> Option<&mut smithay::wayland::drm_syncobj::DrmSyncobjState> {
        self.drm_syncobj_state.as_mut()
    }
}

const MAX_PENDING_DMABUF_IMPORTS: usize = 256;

fn dmabuf_import_queue_has_capacity(pending: usize) -> bool {
    pending < MAX_PENDING_DMABUF_IMPORTS
}

#[derive(Debug)]
struct CancelledCommitBlocker;

impl smithay::wayland::compositor::Blocker for CancelledCommitBlocker {
    fn state(&self) -> smithay::wayland::compositor::BlockerState {
        smithay::wayland::compositor::BlockerState::Cancelled
    }
}

impl Astera {
    pub(super) fn dmabuf_feedback_for_surface(
        &self,
        surface: &WlSurface,
    ) -> Option<DmabufFeedback> {
        let output = self
            .output_runtime
            .iter()
            .find_map(|(output, runtime)| {
                runtime
                    .entered_surfaces
                    .contains(surface)
                    .then_some(*output)
            })
            .or_else(|| {
                self.layers.iter().find_map(|layer| {
                    surface_tree_contains(layer.surface.wl_surface(), surface)
                        .then_some(layer.output)
                })
            })
            .or_else(|| {
                self.windows.iter().find_map(|window| {
                    if !surface_tree_contains(window.surface.wl_surface(), surface) {
                        return None;
                    }
                    self.desktop
                        .find_window(window.id)
                        .ok()
                        .and_then(|workspace| self.desktop.workspace_location(workspace).ok())
                        .and_then(|location| location.output)
                })
            })
            .unwrap_or(self.active_output);
        self.dmabuf_output_feedback.get(&output).cloned()
    }
}

impl OutputHandler for Astera {
    fn output_bound(&mut self, output: SmithayOutput, wl_output: WlOutput) {
        self.workspace_output_bound(&output, wl_output);
    }
}

impl FractionalScaleHandler for Astera {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let Some(scale) = self.output_runtime.iter().find_map(|(output, runtime)| {
            runtime
                .entered_surfaces
                .contains(&surface)
                .then(|| self.output_scale(*output))
        }) else {
            // Background and not-yet-mapped surfaces intentionally have no preferred output.
            return;
        };
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(scale);
            });
        });
    }
}

impl CompositorHandler for Astera {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("all Astera clients have compositor state")
            .compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        smithay::wayland::compositor::add_pre_commit_hook::<Self, _>(
            surface,
            |state, _display, surface| {
                let acquire = with_states(surface, |states| {
                    let mut sync = states.cached_state.get::<DrmSyncobjCachedState>();
                    sync.pending().acquire_point.clone()
                });
                let Some(acquire) = acquire else {
                    return;
                };
                match acquire.generate_blocker() {
                    Ok((blocker, source)) => {
                        smithay::wayland::compositor::add_blocker(surface, blocker);
                        if let Some(client) = surface.client() {
                            state.pending_drm_syncobj_sources.push((client, source));
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "failed to create DRM syncobj commit blocker");
                        // Never apply a buffer whose acquire fence could not be observed. A
                        // cancelled transaction is safer than sampling potentially incomplete
                        // client rendering after a device removal or timeline failure.
                        smithay::wayland::compositor::add_blocker(surface, CancelledCommitBlocker);
                    }
                }
            },
        );
    }

    fn commit(&mut self, surface: &WlSurface) {
        let entered_output = self.output_runtime.iter().find_map(|(output, runtime)| {
            runtime
                .entered_surfaces
                .contains(surface)
                .then_some(*output)
        });
        let was_entered = entered_output.is_some();
        on_commit_buffer_handler::<Self>(surface);
        self.apply_pending_toplevel_icon(surface);
        self.apply_pending_tearing_hint(surface);
        self.apply_pending_color_representation(surface);
        self.track_fifo_barrier(surface, entered_output.unwrap_or(self.active_output));
        self.validate_lock_surface_commit(surface);
        self.popup_manager.commit(surface);
        self.reposition_input_method_popup_surface(surface);
        let committed_buffer =
            with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
        if committed_buffer {
            let configured = self
                .windows
                .iter()
                .find(|window| window.surface.wl_surface() == surface)
                .map(|window| window.surface.ensure_configured())
                .or_else(|| {
                    self.xdg_shell_state
                        .popup_surfaces()
                        .iter()
                        .find(|popup| popup.wl_surface() == surface)
                        .map(PopupSurface::ensure_configured)
                })
                .or_else(|| {
                    self.layers
                        .iter()
                        .find(|mapped| mapped.surface.wl_surface() == surface)
                        .map(|mapped| mapped.surface.layer_surface().ensure_configured())
                });
            if configured == Some(false) {
                // Role-specific ensure_configured posts the protocol error required when a client
                // attaches a buffer before acknowledging its initial configure.
                return;
            }
        }
        // Buffer attachment, damage and subsurface state are visual changes even
        // when the public window metadata remains identical. A role's initial
        // null-buffer commit is protocol setup, however, and must not consume a
        // host frame before the surface maps.
        if committed_buffer
            || was_entered
            || self.is_cursor_surface(surface)
            || self.is_input_method_popup_surface(surface)
        {
            self.mark_render_dirty();
        }
        // Popup and subsurface roots do not have their own mapping callback. Once their first
        // buffer becomes visible, immediately update wl_surface.enter and fractional scale.
        if committed_buffer && !was_entered {
            self.refresh_visible_scales();
        }
        if self
            .lock_surfaces
            .values()
            .any(|lock| lock.wl_surface() == surface)
        {
            self.sync_keyboard_focus();
        }
        // Creating an xdg role is not mapping. The first non-null buffer maps the window and a
        // null-buffer commit unmaps it while preserving the role for a later remap.
        if let Some(index) = self
            .windows
            .iter()
            .position(|window| window.surface.wl_surface() == surface)
        {
            let has_buffer = committed_buffer;
            match (self.windows[index].mapped, has_buffer) {
                (false, true) => self.map_toplevel(index),
                (true, false) => self.unmap_toplevel(index),
                _ => {}
            }
            // A mapped commit may update title/app-id even when buffer presence is unchanged.
            if self.windows[index].mapped {
                self.mark_public_dirty();
            }
        }
        if let Some(layer) = self
            .layers
            .iter()
            .find(|mapped| mapped.surface.wl_surface() == surface)
            .map(|mapped| mapped.surface.clone())
        {
            let was_mapped = self
                .layers
                .iter()
                .find(|mapped| mapped.surface == layer)
                .is_some_and(|mapped| mapped.mapped);
            if let Some(mapped) = self.layers.iter().find(|mapped| mapped.surface == layer)
                && let Some(runtime) = self.output_runtime.get(&mapped.output)
            {
                layer_map_for_output(&runtime.wayland).arrange();
            }
            let has_buffer = committed_buffer;
            if was_mapped && !has_buffer {
                self.cancel_surface_bound_input();
            }
            if let Some(mapped) = self
                .layers
                .iter_mut()
                .find(|mapped| mapped.surface.wl_surface() == surface)
            {
                mapped.mapped = has_buffer;
                mapped.layer = layer.layer();
                if !has_buffer && self.on_demand_layer_focus == Some(mapped.id) {
                    self.on_demand_layer_focus = None;
                }
            }
            self.configure_layer_surface(&layer);
            self.refresh_visible_scales();
            self.sync_keyboard_focus();
            self.mark_public_dirty();
        }
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        self.dmabuf_feedback_surfaces.remove(surface);
        self.pending_toplevel_icons.remove(surface);
        self.remove_tearing_control_surface(surface);
        self.remove_color_representation_surface(surface);
        self.detach_toplevel_drag_surface(surface);
        self.commit_timer_surfaces.remove(surface);
        let was_visible = self
            .output_runtime
            .values()
            .any(|runtime| runtime.entered_surfaces.contains(surface));
        let was_pointer_visual = self.is_cursor_surface(surface);
        if self.dnd_icon.as_ref() == Some(surface) {
            self.dnd_icon = None;
            self.dnd_touch_icon = None;
        }
        if matches!(
            &self.cursor_image_status,
            smithay::input::pointer::CursorImageStatus::Surface(current) if current == surface
        ) {
            // wl_pointer leaves the image undefined after its cursor surface disappears.  Do not
            // retain and repeatedly try to render a dead Wayland resource.
            self.cursor_image_status = smithay::input::pointer::CursorImageStatus::Hidden;
        }
        for runtime in self.tablet_tools.values_mut() {
            if matches!(
                &runtime.cursor_image,
                smithay::input::pointer::CursorImageStatus::Surface(current) if current == surface
            ) {
                runtime.cursor_image = smithay::input::pointer::CursorImageStatus::Hidden;
            }
        }
        // An idle inhibitor can outlive its wl_surface object. The protocol no longer allows it
        // to suppress idle once that surface has left the visible scene, so resume timers now
        // instead of waiting for the inhibitor object itself to be destroyed.
        self.refresh_idle_inhibition();
        if was_visible || was_pointer_visual {
            self.mark_render_dirty();
            self.refresh_visible_scales();
        }
    }
}

impl WlrLayerShellHandler for Astera {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // Only a NULL wl_output permits compositor-selected placement.  A resource for a removed
        // output can outlive its global, but must not silently redirect a panel to another output.
        let output = match output {
            Some(requested) => self
                .output_runtime
                .iter()
                .find_map(|(id, runtime)| runtime.wayland.owns(&requested).then_some(*id)),
            None => self
                .output_runtime
                .contains_key(&self.active_output)
                .then_some(self.active_output)
                .or_else(|| self.output_runtime.keys().next().copied()),
        };
        let Some(output) = output else {
            surface.send_close();
            return;
        };
        let mapped_surface = smithay::desktop::LayerSurface::new(surface.clone(), namespace);
        let id = self.next_layer_id;
        self.next_layer_id = self
            .next_layer_id
            .checked_add(1)
            .expect("layer surface runtime ID space exhausted");
        self.layers.push(MappedLayer {
            id,
            surface: mapped_surface.clone(),
            layer,
            output,
            mapped: false,
        });
        self.mark_public_dirty();
        if let Some(runtime) = self.output_runtime.get(&output)
            && let Err(error) = layer_map_for_output(&runtime.wayland).map_layer(&mapped_surface)
        {
            tracing::warn!(%error, ?output, "could not map layer surface");
        }
        tracing::debug!(?output, ?layer, "layer surface role created");
        self.configure_layer_surface(&mapped_surface);
    }

    fn new_popup(&mut self, _parent: LayerSurface, popup: PopupSurface) {
        // An xdg popup used by layer-shell is initially created without an xdg parent.  Its real
        // root only becomes discoverable when get_popup assigns the layer surface here, so the
        // generic XDG new_popup callback cannot constrain it correctly on its first pass.
        let positioner = popup.with_pending_state(|state| state.positioner);
        let geometry = self.constrain_popup_geometry(&popup, positioner);
        popup.with_pending_state(|state| state.geometry = geometry);
        if let Err(error) = self.popup_manager.track_popup(popup.clone().into()) {
            tracing::warn!(%error, "could not track layer popup");
            return;
        }
        if let Err(error) = popup.send_configure() {
            tracing::warn!(%error, "could not configure layer popup");
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        self.cancel_surface_bound_input();
        if let Some(mapped) = self
            .layers
            .iter()
            .find(|mapped| mapped.surface.layer_surface() == &surface)
            && let Some(runtime) = self.output_runtime.get(&mapped.output)
        {
            layer_map_for_output(&runtime.wayland).unmap_layer(&mapped.surface);
        }
        if let Some(id) = self
            .layers
            .iter()
            .find(|mapped| mapped.surface.layer_surface() == &surface)
            .map(|mapped| mapped.id)
            && self.on_demand_layer_focus == Some(id)
        {
            self.on_demand_layer_focus = None;
        }
        self.layers
            .retain(|mapped| mapped.surface.layer_surface() != &surface);
        self.mark_public_dirty();
        tracing::debug!("layer surface destroyed");
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
        self.handle_pointer_motion(self.pointer_location, 0);
    }
}

impl XdgShellHandler for Astera {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let id = WindowId(self.next_window_id);
        self.next_window_id = self
            .next_window_id
            .checked_add(1)
            .expect("window runtime ID space exhausted");
        surface.with_pending_state(|state| {
            state.size = Some(
                (
                    DEFAULT_WINDOW_SIZE.width as i32,
                    DEFAULT_WINDOW_SIZE.height as i32,
                )
                    .into(),
            );
        });
        surface.send_configure();
        let (title, app_id) = xdg_toplevel_metadata(&surface);
        let foreign_toplevel = self
            .foreign_toplevel_list_state
            .new_toplevel::<Self>(title, app_id);
        self.windows.push(MappedWindow {
            id,
            surface,
            mapped: false,
            initial_mode: None,
            urgent: false,
            tag: None,
            description: None,
            icon_name: None,
            icon_buffers: Vec::new(),
            foreign_toplevel,
        });
        tracing::debug!(window = ?id, "toplevel role created");
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        let parent = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|role| role.lock().ok()?.parent.clone())
        });
        if parent.as_ref().is_some_and(|parent| {
            !self
                .windows
                .iter()
                .any(|window| window.mapped && window.surface.wl_surface() == parent)
        }) {
            with_states(surface.wl_surface(), |states| {
                if let Some(role) = states.data_map.get::<XdgToplevelSurfaceData>()
                    && let Ok(mut role) = role.lock()
                {
                    role.parent = None;
                }
            });
            xdg_foreign::remove_child_relationship(self, surface.wl_surface());
        }
        self.mark_public_dirty();
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let geometry = self.constrain_popup_geometry(&surface, positioner);
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = geometry;
        });
        if let Err(error) = self.popup_manager.track_popup(surface.clone().into()) {
            tracing::warn!(%error, "could not track xdg popup");
            return;
        }
        if let Err(error) = surface.send_configure() {
            tracing::warn!(%error, "could not configure xdg popup");
        }
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        if !self.seat.owns(&seat) || self.drag.is_some() {
            return;
        }
        let Some((source, location)) = self.interactive_grab(&surface, serial) else {
            return;
        };
        let Some(requested) = self
            .windows
            .iter()
            .find(|window| window.mapped && window.surface == surface)
            .map(|window| window.id)
        else {
            return;
        };
        self.begin_drag(Some((requested, source, location)));
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        if !self.seat.owns(&seat) || self.drag.is_some() {
            return;
        }
        let Some((source, location)) = self.interactive_grab(&surface, serial) else {
            return;
        };
        let Some(requested) = self
            .windows
            .iter()
            .find(|window| window.mapped && window.surface == surface)
            .map(|window| window.id)
        else {
            return;
        };
        let edges = match edges {
            xdg_toplevel::ResizeEdge::Top => ResizeEdges {
                top: true,
                bottom: false,
                left: false,
                right: false,
            },
            xdg_toplevel::ResizeEdge::Bottom => ResizeEdges {
                top: false,
                bottom: true,
                left: false,
                right: false,
            },
            xdg_toplevel::ResizeEdge::Left => ResizeEdges {
                top: false,
                bottom: false,
                left: true,
                right: false,
            },
            xdg_toplevel::ResizeEdge::Right => ResizeEdges {
                top: false,
                bottom: false,
                left: false,
                right: true,
            },
            xdg_toplevel::ResizeEdge::TopLeft => ResizeEdges {
                top: true,
                bottom: false,
                left: true,
                right: false,
            },
            xdg_toplevel::ResizeEdge::BottomLeft => ResizeEdges {
                top: false,
                bottom: true,
                left: true,
                right: false,
            },
            xdg_toplevel::ResizeEdge::TopRight => ResizeEdges {
                top: true,
                bottom: false,
                left: false,
                right: true,
            },
            xdg_toplevel::ResizeEdge::BottomRight => ResizeEdges {
                top: false,
                bottom: true,
                left: false,
                right: true,
            },
            xdg_toplevel::ResizeEdge::None => return,
            _ => return,
        };
        self.begin_resize(requested, edges, source, location);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if self.queue_initial_toplevel_mode(&surface, Some(WindowMode::Maximized)) {
            return;
        }
        self.apply_toplevel_mode_request(&surface, WindowMode::Maximized);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if self.clear_initial_toplevel_mode(&surface, WindowMode::Maximized) {
            return;
        }
        let Some(window) = self
            .windows
            .iter()
            .find(|window| window.mapped && window.surface == surface)
            .map(|window| window.id)
        else {
            return;
        };
        let Ok(workspace_id) = self.desktop.find_window(window) else {
            return;
        };
        let Some(target) = self
            .desktop
            .workspace(workspace_id)
            .ok()
            .and_then(|workspace| workspace.maximized.as_ref())
            .filter(|maximized| maximized.window == window)
            .map(|maximized| match maximized.restore {
                RestorePlacement::Tiled { .. } => WindowMode::Tiled,
                RestorePlacement::Floating { .. } => WindowMode::Floating,
            })
        else {
            return;
        };
        self.apply_toplevel_mode_request(&surface, target);
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.apply_toplevel_mode_request(&surface, WindowMode::Minimized);
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
        if let Some(index) = self
            .windows
            .iter()
            .position(|window| !window.mapped && window.surface == surface)
        {
            if let Some(requested) = output
                && !self
                    .output_runtime
                    .get(&self.active_output)
                    .is_some_and(|runtime| runtime.wayland.owns(&requested))
            {
                tracing::warn!("initial fullscreen request targeted an inactive output");
                surface.send_configure();
                return;
            }
            self.windows[index].initial_mode = Some(WindowMode::Fullscreen);
            self.configure_initial_toplevel_mode(index);
            return;
        }
        let Some(window) = self
            .windows
            .iter()
            .find(|window| window.mapped && window.surface == surface)
            .map(|window| window.id)
        else {
            return;
        };
        let Ok(workspace) = self.desktop.find_window(window) else {
            return;
        };
        if let Some(requested) = output
            && !self.output_runtime.iter().any(|(output, runtime)| {
                runtime.wayland.owns(&requested)
                    && self.desktop.active_workspace_id(*output) == Some(workspace)
            })
        {
            tracing::warn!(
                ?window,
                "fullscreen request targeted another workspace output"
            );
            return;
        }
        let Some(viewport_size) = self
            .desktop
            .workspace_location(workspace)
            .ok()
            .and_then(|location| location.output)
            .and_then(|output| self.desktop.output(output))
            .map(|output| output.logical_size)
        else {
            return;
        };
        if self
            .desktop
            .apply_window(
                workspace,
                WindowTransaction::SetMode {
                    id: window,
                    mode: WindowMode::Fullscreen,
                    viewport_size,
                },
            )
            .is_ok()
        {
            self.configure_window_mode(window, WindowMode::Fullscreen);
            self.refresh_visible_scales();
            self.sync_keyboard_focus();
            self.mark_public_dirty();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if self.clear_initial_toplevel_mode(&surface, WindowMode::Fullscreen) {
            return;
        }
        let Some(window) = self
            .windows
            .iter()
            .find(|window| window.mapped && window.surface == surface)
            .map(|window| window.id)
        else {
            return;
        };
        let Ok(workspace) = self.desktop.find_window(window) else {
            return;
        };
        if self
            .desktop
            .workspace(workspace)
            .ok()
            .and_then(|workspace| workspace.window_mode(window))
            != Some(WindowMode::Fullscreen)
        {
            // unset_fullscreen is not a toggle. Repeating it, or issuing it for a window that was
            // never fullscreen, must not enter fullscreen as a side effect.
            return;
        }
        let Ok(mode) = self.toggle_fullscreen_mode(window) else {
            return;
        };
        let Some(viewport_size) = self
            .desktop
            .workspace_location(workspace)
            .ok()
            .and_then(|location| location.output)
            .and_then(|output| self.desktop.output(output))
            .map(|output| output.logical_size)
        else {
            return;
        };
        if self
            .desktop
            .apply_window(
                workspace,
                WindowTransaction::SetMode {
                    id: window,
                    mode,
                    viewport_size,
                },
            )
            .is_ok()
        {
            self.configure_window_mode(window, mode);
            self.refresh_visible_scales();
            self.sync_keyboard_focus();
            self.mark_public_dirty();
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat_resource: wl_seat::WlSeat, serial: Serial) {
        let popup = PopupKind::from(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let root_client = root.client().map(|client| client.id());
        let pointer_start = self.pointer.grab_start_data();
        let touch_start = self.touch.grab_start_data();
        let pointer_authorized = self.pointer.has_grab(serial)
            && pointer_start.as_ref().is_some_and(|start| {
                start
                    .focus
                    .as_ref()
                    .and_then(|(surface, _)| surface.client())
                    .map(|client| client.id())
                    == root_client
            });
        let touch_authorized = self.touch.has_grab(serial)
            && touch_start.as_ref().is_some_and(|start| {
                start
                    .focus
                    .as_ref()
                    .and_then(|(surface, _)| surface.client())
                    .map(|client| client.id())
                    == root_client
            });
        let grab_source = popup_grab_source(pointer_authorized, touch_authorized);
        let authorized = self.seat.owns(&seat_resource)
            && grab_source.is_some()
            && root_client.as_ref().is_some_and(|client| {
                self.activation_tracker
                    .authorizes_input(serial, client, self.clock.now())
            });
        if !authorized {
            // xdg-shell requires denied grabs to be dismissed immediately. In particular, never
            // install a compositor grab for a guessed, expired, foreign-client, or foreign-seat
            // serial: input serials are capabilities, not arbitrary sequence numbers.
            let _ = PopupManager::dismiss_popup(&root, &popup);
            return;
        }
        let seat = self.seat.clone();
        let Ok(grab) = self
            .popup_manager
            .grab_popup::<Self>(root, popup, &seat, serial)
        else {
            return;
        };
        let keyboard = self.keyboard.clone();
        keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        match grab_source {
            Some(PopupGrabSource::Pointer) => {
                let pointer = self.pointer.clone();
                pointer.set_grab(
                    self,
                    PopupPointerGrab::new(&grab),
                    serial,
                    PointerFocusMode::Keep,
                );
            }
            Some(PopupGrabSource::Touch) => {
                let touch = self.touch.clone();
                touch.set_grab(
                    self,
                    popup_touch::PopupTouchGrab::new(grab, touch_start.unwrap()),
                    serial,
                );
            }
            None => unreachable!("unauthorized popup grabs returned above"),
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.cancel_surface_bound_input();
        self.reparent_children_of_unmapped_toplevel(surface.wl_surface());
        self.detach_toplevel_drag_surface(surface.wl_surface());
        let Some(index) = self
            .windows
            .iter()
            .position(|window| window.surface == surface)
        else {
            return;
        };
        let window = self.windows.remove(index);
        self.foreign_toplevel_list_state
            .remove_toplevel(&window.foreign_toplevel);
        if window.mapped
            && let Ok(workspace) = self.desktop.find_window(window.id)
        {
            let _ = self
                .desktop
                .apply_window(workspace, WindowTransaction::Remove { id: window.id });
        }
        if self.drag.is_some_and(|drag| drag.window == window.id) {
            self.cancel_drag();
        }
        tracing::info!(window = ?window.id, "toplevel role destroyed");
        self.mark_public_dirty();
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
        self.handle_pointer_motion(self.pointer_location, 0);
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.cancel_pointer_gesture(0);
        self.popup_manager.cleanup();
        self.mark_render_dirty();
        self.refresh_visible_scales();
        self.handle_pointer_motion(self.pointer_location, 0);
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        self.update_foreign_toplevel_metadata(&surface);
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        self.update_foreign_toplevel_metadata(&surface);
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let geometry = self.constrain_popup_geometry(&surface, positioner);
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = geometry;
        });
        // Smithay emits the complete repositioned -> popup.configure -> xdg_surface.configure
        // sequence and records its acknowledgement serial in this call.
        surface.send_repositioned(token);
    }
}

impl Astera {
    fn update_foreign_toplevel_metadata(&mut self, surface: &ToplevelSurface) {
        let Some(handle) = self
            .windows
            .iter()
            .find(|window| window.surface == *surface)
            .map(|window| window.foreign_toplevel.clone())
        else {
            return;
        };
        let (title, app_id) = xdg_toplevel_metadata(surface);
        handle.send_title(&title);
        handle.send_app_id(&app_id);
        handle.send_done();
        self.mark_public_dirty();
    }

    fn interactive_grab(
        &self,
        surface: &ToplevelSurface,
        serial: Serial,
    ) -> Option<(DragSource, SmithayPoint<f64, Logical>)> {
        let pointer = if self.pointer.has_grab(serial)
            && let Some(start) = self.pointer.grab_start_data()
            && implicit_grab_authorizes_surface(true, start.focus.as_ref(), |(origin, _)| {
                surface_tree_contains(surface.wl_surface(), origin)
            }) {
            Some((DragSource::Pointer, start.location))
        } else {
            None
        };
        let touch = if self.touch.has_grab(serial)
            && let Some(start) = self.touch.grab_start_data()
            && implicit_grab_authorizes_surface(true, start.focus.as_ref(), |(origin, _)| {
                surface_tree_contains(surface.wl_surface(), origin)
            }) {
            Some((DragSource::Touch(start.slot), start.location))
        } else {
            None
        };
        select_interactive_grab(pointer, touch)
    }

    pub(super) fn reconstrain_reactive_popups(&mut self) {
        // Collect first because calculating a popup target reads the complete desktop/layer scene,
        // while sending a configure mutates protocol state.
        let roots = self
            .windows
            .iter()
            .map(|window| window.surface.wl_surface().clone())
            .chain(
                self.layers
                    .iter()
                    .map(|layer| layer.surface.wl_surface().clone()),
            )
            .collect::<Vec<_>>();
        let popups = roots
            .iter()
            .flat_map(PopupManager::popups_for_surface)
            .filter_map(|(popup, _)| match popup {
                PopupKind::Xdg(popup) => Some(popup),
                PopupKind::InputMethod(_) => None,
            })
            .collect::<Vec<_>>();

        for popup in popups {
            let (positioner, previous) =
                popup.with_pending_state(|state| (state.positioner, state.geometry));
            if !positioner.reactive {
                continue;
            }
            let geometry = self.constrain_popup_geometry(&popup, positioner);
            if geometry == previous {
                continue;
            }
            popup.with_pending_state(|state| state.geometry = geometry);
            if let Err(error) = popup.send_pending_configure() {
                tracing::warn!(%error, "could not reconfigure reactive xdg popup");
            }
        }
    }

    /// Apply xdg-positioner's flip/slide/resize rules in coordinates relative to the popup's
    /// immediate parent window geometry. PopupManager reports nested popup offsets relative to
    /// the root, which keeps this correct for submenu trees as well as direct toplevel children.
    fn constrain_popup_geometry(
        &self,
        surface: &PopupSurface,
        positioner: PositionerState,
    ) -> SmithayRectangle<i32, Logical> {
        let fallback = positioner.get_geometry();
        let popup = PopupKind::from(surface.clone());
        let Ok(root) = find_popup_root_surface(&popup) else {
            return fallback;
        };
        let Some(parent) = surface.get_parent_surface() else {
            return fallback;
        };

        let parent_offset = if parent == root {
            SmithayPoint::from((0, 0))
        } else {
            let Some((_, offset)) = PopupManager::popups_for_surface(&root)
                .find(|(popup, _)| popup.wl_surface() == &parent)
            else {
                return fallback;
            };
            offset
        };

        let window_root = self
            .windows
            .iter()
            .find(|window| window.mapped && window.surface.wl_surface() == &root)
            .and_then(|window| {
                let workspace = self.desktop.find_window(window.id).ok()?;
                let output = self.desktop.workspace_location(workspace).ok()?.output?;
                let (origin, _, scale, _) = self.visual_geometry_for_output(output, window.id)?;
                Some((output, origin, scale))
            });
        let layer_root = self
            .layers
            .iter()
            // Layer popups are assigned before the parent attaches its first buffer. The layer
            // map already has authoritative geometry at that point, so mapping is not required
            // to constrain the popup's initial configure.
            .find(|layer| layer.surface.wl_surface() == &root)
            .and_then(|layer| {
                let (origin, _) = self.layer_geometry(layer)?;
                Some((layer.output, origin, 1.0))
            });
        let Some((output, origin, scale)) = window_root.or(layer_root) else {
            return fallback;
        };
        let Some(output_size) = self
            .desktop
            .output(output)
            .map(|output| output.logical_size)
        else {
            return fallback;
        };

        let parent_x = origin.x as f64 / scale + f64::from(parent_offset.x);
        let parent_y = origin.y as f64 / scale + f64::from(parent_offset.y);
        let target = SmithayRectangle::new(
            ((-parent_x).round() as i32, (-parent_y).round() as i32).into(),
            (
                (output_size.width as f64 / scale).round() as i32,
                (output_size.height as f64 / scale).round() as i32,
            )
                .into(),
        );
        positioner.get_unconstrained_geometry(target)
    }

    fn queue_initial_toplevel_mode(
        &mut self,
        surface: &ToplevelSurface,
        mode: Option<WindowMode>,
    ) -> bool {
        let Some(index) = self
            .windows
            .iter()
            .position(|window| !window.mapped && &window.surface == surface)
        else {
            return false;
        };
        self.windows[index].initial_mode = mode;
        self.configure_initial_toplevel_mode(index);
        true
    }

    fn clear_initial_toplevel_mode(
        &mut self,
        surface: &ToplevelSurface,
        expected: WindowMode,
    ) -> bool {
        let Some(index) = self
            .windows
            .iter()
            .position(|window| !window.mapped && &window.surface == surface)
        else {
            return false;
        };
        if self.windows[index].initial_mode == Some(expected) {
            self.windows[index].initial_mode = None;
        }
        self.configure_initial_toplevel_mode(index);
        true
    }

    fn configure_initial_toplevel_mode(&self, index: usize) {
        let mode = self.windows[index].initial_mode;
        let size = match mode {
            Some(WindowMode::Fullscreen) => self
                .desktop
                .output(self.active_output)
                .map(|output| output.logical_size),
            Some(WindowMode::Maximized) => {
                self.usable_rect(self.active_output).map(|rect| rect.size)
            }
            _ => Some(DEFAULT_WINDOW_SIZE),
        };
        let surface = &self.windows[index].surface;
        surface.with_pending_state(|state| {
            state.size =
                size.map(|size| (saturating_i32(size.width), saturating_i32(size.height)).into());
            if mode == Some(WindowMode::Fullscreen) {
                state.states.set(xdg_toplevel::State::Fullscreen);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
            if mode == Some(WindowMode::Maximized) {
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Maximized);
            }
        });
        surface.send_pending_configure();
    }

    fn apply_toplevel_mode_request(&mut self, surface: &ToplevelSurface, mode: WindowMode) {
        let Some(window) = self
            .windows
            .iter()
            .find(|window| window.mapped && &window.surface == surface)
            .map(|window| window.id)
        else {
            return;
        };
        let Ok(workspace) = self.desktop.find_window(window) else {
            return;
        };
        let Some(viewport_size) = self
            .desktop
            .workspace_location(workspace)
            .ok()
            .and_then(|location| location.output)
            .and_then(|output| self.desktop.output(output))
            .map(|output| output.logical_size)
        else {
            return;
        };
        match self.desktop.apply_window(
            workspace,
            WindowTransaction::SetMode {
                id: window,
                mode,
                viewport_size,
            },
        ) {
            Ok(()) => {
                self.configure_window_mode(window, mode);
                self.refresh_visible_scales();
                self.sync_keyboard_focus();
                self.mark_public_dirty();
            }
            Err(error) => {
                tracing::warn!(?window, ?mode, %error, "xdg toplevel mode request rejected");
                // The request did not change compositor state.  A configure makes the current
                // state authoritative so clients do not wait indefinitely for an accepted mode.
                surface.send_configure();
            }
        }
    }
}

impl ShmHandler for Astera {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for Astera {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        self.update_shortcut_inhibitor(seat, focused);
        let client = focused.and_then(Resource::client);
        set_data_device_focus(&self.display, seat, client.clone());
        set_primary_focus(&self.display, seat, client);
    }

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        self.update_named_cursor(&image);
        self.cursor_image_status = image;
        self.mark_render_dirty();
        self.refresh_visible_scales();
    }
}

impl TabletSeatHandler for Astera {
    fn tablet_tool_image(
        &mut self,
        tool: &smithay::backend::input::TabletToolDescriptor,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        self.update_named_cursor(&image);
        if let Some(runtime) = self.tablet_tools.get_mut(tool) {
            runtime.cursor_image = image;
        }
        self.mark_render_dirty();
        self.refresh_visible_scales();
    }
}

impl InputMethodHandler for Astera {
    fn new_popup(&mut self, surface: InputMethodPopupSurface) {
        self.add_input_method_popup(surface);
    }

    fn dismiss_popup(&mut self, surface: InputMethodPopupSurface) {
        self.remove_input_method_popup(&surface);
    }

    fn popup_repositioned(&mut self, surface: InputMethodPopupSurface) {
        self.reposition_input_method_popup(&surface);
    }

    fn parent_geometry(
        &self,
        parent: &WlSurface,
    ) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        self.input_method_parent_geometry(parent)
            .map(|(_, geometry)| geometry)
            .unwrap_or_default()
    }
}

impl Astera {
    pub(super) fn update_shortcut_inhibitor(
        &mut self,
        seat: &Seat<Self>,
        focused: Option<&WlSurface>,
    ) {
        if let Some(inhibitor) = self.active_shortcut_inhibitor.take() {
            if Some(inhibitor.wl_surface()) == focused {
                self.active_shortcut_inhibitor = Some(inhibitor);
                return;
            }
            inhibitor.inactivate();
        }
        if let Some(focused) = focused
            && let Some(inhibitor) = seat.keyboard_shortcuts_inhibitor_for_surface(focused)
        {
            if !inhibitor.is_active() {
                inhibitor.activate();
            }
            self.active_shortcut_inhibitor = Some(inhibitor);
            self.key_repeat.cancel_repeats();
        }
    }
}

impl KeyboardShortcutsInhibitHandler for Astera {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        if self.keyboard.current_focus().as_ref() == Some(inhibitor.wl_surface()) {
            inhibitor.activate();
            self.active_shortcut_inhibitor = Some(inhibitor);
            self.key_repeat.cancel_repeats();
        }
    }

    fn inhibitor_destroyed(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        if self.active_shortcut_inhibitor.as_ref() == Some(&inhibitor) {
            self.active_shortcut_inhibitor = None;
        }
    }
}

impl SelectionHandler for Astera {
    type SelectionUserData = ();
}

impl ExtDataControlHandler for Astera {
    fn data_control_state(&self) -> &ExtDataControlState {
        &self.ext_data_control_state
    }
}

impl SecurityContextHandler for Astera {
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        self.pending_security_contexts.push((source, context));
    }
}

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timing_manager_v1::WpCommitTimingManagerV1,
        bool,
    > for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timing_manager_v1::WpCommitTimingManagerV1,
        request: smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timing_manager_v1::Request,
        data: &bool,
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timing_manager_v1::Request;
        if let Request::GetTimer { surface, .. } = &request {
            state.commit_timer_surfaces.insert(surface.clone());
        }
        <CommitTimingManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timer_v1::WpCommitTimerV1,
        smithay::reexports::wayland_server::Weak<WlSurface>,
    > for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timer_v1::WpCommitTimerV1,
        request: smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timer_v1::Request,
        data: &smithay::reexports::wayland_server::Weak<WlSurface>,
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timer_v1::{Error, Request};
        if let Request::SetTimestamp { tv_nsec, .. } = &request
            && *tv_nsec >= 1_000_000_000
        {
            resource.post_error(
                Error::InvalidTimestamp as u32,
                "commit timestamp nanoseconds must be below one billion".to_string(),
            );
            return;
        }
        <CommitTimingManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl DataDeviceHandler for Astera {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for Astera {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
        smithay::wayland::selection::primary_selection::PrimaryDeviceUserData,
    > for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
        request: smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_v1::Request,
        data: &smithay::wayland::selection::primary_selection::PrimaryDeviceUserData,
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_v1::Request;
        if let Request::SetSelection { serial, .. } = &request {
            let serial = Serial::from(*serial);
            let client_id = client.id();
            let focused = state
                .keyboard
                .current_focus()
                .and_then(|surface| surface.client())
                .is_some_and(|focused| focused.id() == client_id);
            let authorized = focused
                && state
                    .activation_tracker
                    .authorizes_input(serial, &client_id, state.clock.now())
                && state
                    .last_primary_selection_serial
                    .is_none_or(|last| serial.is_no_older_than(&last));
            if !authorized {
                tracing::debug!(
                    ?serial,
                    ?client_id,
                    "denying primary selection with invalid input serial"
                );
                return;
            }
            state.last_primary_selection_serial = Some(serial);
        }
        <PrimarySelectionState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
        data: &smithay::wayland::selection::primary_selection::PrimaryDeviceUserData,
    ) {
        <PrimarySelectionState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_server::protocol::wl_data_device::WlDataDevice,
        smithay::wayland::selection::data_device::DataDeviceUserData,
    > for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_server::protocol::wl_data_device::WlDataDevice,
        request: smithay::reexports::wayland_server::protocol::wl_data_device::Request,
        data: &smithay::wayland::selection::data_device::DataDeviceUserData,
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_server::protocol::wl_data_device::Request;
        let mut touch_icon = None;
        let mut update_touch_icon = false;
        let mut client_dnd_input = None;
        if let Request::StartDrag { origin, serial, .. } = &request {
            update_touch_icon = true;
            let serial = Serial::from(*serial);
            let pointer_authorized = implicit_grab_authorizes_origin(
                state.pointer.has_grab(serial),
                state
                    .pointer
                    .grab_start_data()
                    .and_then(|start| start.focus.map(|(surface, _)| surface))
                    .as_ref(),
                origin,
            );
            let touch_authorized = implicit_grab_authorizes_origin(
                state.touch.has_grab(serial),
                state
                    .touch
                    .grab_start_data()
                    .and_then(|start| start.focus.map(|(surface, _)| surface))
                    .as_ref(),
                origin,
            );
            if !pointer_authorized && !touch_authorized {
                tracing::debug!(
                    ?serial,
                    "denying drag whose origin did not receive the grab"
                );
                return;
            }
            client_dnd_input = Some(if pointer_authorized {
                ClientDndInput::Pointer
            } else {
                ClientDndInput::Touch
            });
            touch_icon = if touch_authorized && !pointer_authorized {
                state.touch.grab_start_data().and_then(|start| {
                    state
                        .touch_slots
                        .values()
                        .find(|(_, slot)| *slot == start.slot)
                        .map(|(output, _)| (*output, start.slot, start.location))
                })
            } else {
                None
            };
        }
        if let Request::SetSelection {
            source: Some(source),
            ..
        } = &request
            && state.reject_toplevel_drag_selection(source)
        {
            return;
        }
        if let Request::SetSelection { serial, .. } = &request {
            let serial = Serial::from(*serial);
            let client_id = client.id();
            let focused = state
                .keyboard
                .current_focus()
                .and_then(|surface| surface.client())
                .is_some_and(|focused| focused.id() == client_id);
            let authorized = focused
                && state
                    .activation_tracker
                    .authorizes_input(serial, &client_id, state.clock.now())
                && state
                    .last_selection_serial
                    .is_none_or(|last| serial.is_no_older_than(&last));
            if !authorized {
                tracing::debug!(
                    ?serial,
                    ?client_id,
                    "denying selection with invalid input serial"
                );
                return;
            }
            state.last_selection_serial = Some(serial);
        }
        if let Request::SetSelection {
            source: Some(source),
            ..
        } = &request
        {
            state.used_selection_sources.insert(source.clone());
        }
        state.pending_client_dnd_input = client_dnd_input;
        <DataDeviceState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
        state.pending_client_dnd_input = None;
        if update_touch_icon {
            state.dnd_touch_icon = state.dnd_icon.is_some().then_some(touch_icon).flatten();
            if state.dnd_touch_icon.is_some() {
                // `ClientDndGrabHandler::started` runs inside the delegated request before this
                // wrapper can identify the touch slot. Recompute enter/leave and preferred scale
                // now that the icon's real output is known instead of leaving it on the pointer
                // output until an unrelated scene change.
                state.mark_render_dirty();
                state.refresh_visible_scales();
            }
        }
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &smithay::reexports::wayland_server::protocol::wl_data_device::WlDataDevice,
        data: &smithay::wayland::selection::data_device::DataDeviceUserData,
    ) {
        <DataDeviceState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

fn implicit_grab_authorizes_origin<T: PartialEq>(
    serial_matches: bool,
    focused: Option<&T>,
    origin: &T,
) -> bool {
    implicit_grab_authorizes_surface(serial_matches, focused, |focused| focused == origin)
}

fn implicit_grab_authorizes_surface<T>(
    serial_matches: bool,
    focused: Option<&T>,
    belongs_to_surface_tree: impl FnOnce(&T) -> bool,
) -> bool {
    serial_matches && focused.is_some_and(belongs_to_surface_tree)
}

fn select_interactive_grab<T>(pointer: Option<T>, touch: Option<T>) -> Option<T> {
    pointer.or(touch)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PopupGrabSource {
    Pointer,
    Touch,
}

fn popup_grab_source(pointer: bool, touch: bool) -> Option<PopupGrabSource> {
    if pointer {
        Some(PopupGrabSource::Pointer)
    } else if touch {
        Some(PopupGrabSource::Touch)
    } else {
        None
    }
}

#[cfg(test)]
mod data_device_tests {
    use super::{
        MAX_PENDING_DMABUF_IMPORTS, PopupGrabSource, dmabuf_import_queue_has_capacity,
        implicit_grab_authorizes_origin, implicit_grab_authorizes_surface, popup_grab_source,
        select_interactive_grab,
    };

    #[test]
    fn drag_origin_must_be_the_surface_that_received_the_implicit_grab() {
        assert!(implicit_grab_authorizes_origin(
            true,
            Some(&"origin"),
            &"origin"
        ));
        assert!(!implicit_grab_authorizes_origin(
            true,
            Some(&"other"),
            &"origin"
        ));
        assert!(!implicit_grab_authorizes_origin(
            false,
            Some(&"origin"),
            &"origin"
        ));
        assert!(!implicit_grab_authorizes_origin::<&str>(
            true, None, &"origin"
        ));
    }

    #[test]
    fn interactive_toplevel_request_must_own_the_grab_origin_tree() {
        assert!(implicit_grab_authorizes_surface(
            true,
            Some(&"window/subsurface"),
            |focused| focused.starts_with("window/")
        ));
        assert!(!implicit_grab_authorizes_surface(
            true,
            Some(&"other/surface"),
            |focused| focused.starts_with("window/")
        ));
        assert!(!implicit_grab_authorizes_surface(
            false,
            Some(&"window/subsurface"),
            |focused| focused.starts_with("window/")
        ));
    }

    #[test]
    fn interactive_toplevel_request_accepts_touch_grab_fallback() {
        assert_eq!(select_interactive_grab(None, Some("touch")), Some("touch"));
        assert_eq!(
            select_interactive_grab(Some("pointer"), Some("touch")),
            Some("pointer")
        );
    }

    #[test]
    fn popup_grab_requires_pointer_or_touch_implicit_grab() {
        assert_eq!(
            popup_grab_source(true, true),
            Some(PopupGrabSource::Pointer)
        );
        assert_eq!(popup_grab_source(false, true), Some(PopupGrabSource::Touch));
        assert_eq!(popup_grab_source(false, false), None);
    }

    #[test]
    fn dmabuf_import_queue_rejects_the_first_excess_resource() {
        assert!(dmabuf_import_queue_has_capacity(
            MAX_PENDING_DMABUF_IMPORTS - 1
        ));
        assert!(!dmabuf_import_queue_has_capacity(
            MAX_PENDING_DMABUF_IMPORTS
        ));
        assert!(!dmabuf_import_queue_has_capacity(usize::MAX));
    }
}

impl ClientDndGrabHandler for Astera {
    fn started(
        &mut self,
        source: Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.active_client_dnd_input = self.pending_client_dnd_input.take();
        self.dnd_icon = icon;
        self.mark_render_dirty();
        self.refresh_visible_scales();
        if let Some(source) = source {
            self.start_toplevel_drag(&source);
        }
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.active_client_dnd_input = None;
        self.finish_toplevel_drag();
        self.dnd_icon = None;
        self.dnd_touch_icon = None;
        self.mark_render_dirty();
        self.refresh_visible_scales();
    }
}

impl
    smithay::reexports::wayland_server::Dispatch<
        smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource,
        smithay::wayland::selection::data_device::DataSourceUserData,
    > for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource,
        request: smithay::reexports::wayland_server::protocol::wl_data_source::Request,
        data: &smithay::wayland::selection::data_device::DataSourceUserData,
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        <DataDeviceState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource,
        data: &smithay::wayland::selection::data_device::DataSourceUserData,
    ) {
        state.remove_toplevel_drag_source(resource);
        <DataDeviceState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

impl ServerDndGrabHandler for Astera {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl ForeignToplevelListHandler for Astera {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list_state
    }
}

delegate_xdg_shell!(Astera);
delegate_foreign_toplevel_list!(Astera);
delegate_xdg_decoration!(Astera);
smithay::reexports::wayland_server::delegate_global_dispatch!(Astera: [
    smithay::reexports::wayland_protocols::xdg::dialog::v1::server::xdg_wm_dialog_v1::XdgWmDialogV1: ()
] => smithay::wayland::shell::xdg::dialog::XdgDialogState);
delegate_xdg_activation!(Astera);
delegate_idle_inhibit!(Astera);
delegate_keyboard_shortcuts_inhibit!(Astera);
delegate_pointer_gestures!(Astera);
delegate_tablet_manager!(Astera);
delegate_cursor_shape!(Astera);
smithay::delegate_text_input_manager!(Astera);
smithay::reexports::wayland_server::delegate_global_dispatch!(Astera: [
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2:
    smithay::wayland::input_method::InputMethodManagerGlobalData
] => smithay::wayland::input_method::InputMethodManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Astera: [
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2:
    smithay::wayland::input_method::InputMethodKeyboardUserData<Self>
] => smithay::wayland::input_method::InputMethodManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Astera: [
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2:
    smithay::wayland::input_method::InputMethodPopupSurfaceUserData
] => smithay::wayland::input_method::InputMethodManagerState);

impl smithay::reexports::wayland_server::Dispatch<
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    (),
> for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        request: smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::Request;
        if state.session_is_locked() {
            match request {
                Request::GetInputMethod { input_method, .. } => {
                    let input_method = data_init.init(input_method, ());
                    input_method.unavailable();
                    return;
                }
                request => {
                    return <InputMethodManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
                        state, client, resource, request, data, display, data_init,
                    );
                }
            }
        } else if state.input_method_claimed {
            match request {
                Request::GetInputMethod { input_method, .. } => {
                    // input-method-v2 requires a second instance to be created and receive the sole
                    // `unavailable` event. It is not a protocol error on the manager or connection.
                    let input_method = data_init.init(input_method, ());
                    input_method.unavailable();
                    return;
                }
                request => {
                    return <InputMethodManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
                        state, client, resource, request, data, display, data_init,
                    );
                }
            }
        } else if matches!(&request, Request::GetInputMethod { .. }) {
            state.input_method_claimed = true;
            // Track the privileged connection at object creation time. It may otherwise enter a
            // session lock before sending any request on the new input-method object, leaving no
            // resource for the lock transition to revoke.
            state.input_method_client = Some(client.clone());
            state.input_method_manager_resource = Some(resource.clone());
        }
        <InputMethodManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }
}

impl smithay::reexports::wayland_server::Dispatch<
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
    (),
> for Astera
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
        request: smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::Request;

        // Objects rejected with `unavailable` are inert for the rest of their lifetime. Requests
        // carrying new IDs still need inert server resources: leaving a New uninitialized makes
        // wayland-server panic instead of ignoring the request as the protocol requires.
        match request {
            Request::GetInputPopupSurface { id, .. } => {
                data_init.init(id, ());
            }
            Request::GrabKeyboard { keyboard } => {
                data_init.init(keyboard, ());
            }
            _ => {}
        }
    }
}

impl smithay::reexports::wayland_server::Dispatch<
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
    (),
> for Astera
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
        _request: smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_popup_surface_v2::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
    }
}

impl smithay::reexports::wayland_server::Dispatch<
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    (),
> for Astera
{
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
        _request: smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_keyboard_grab_v2::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
    }
}

impl smithay::reexports::wayland_server::Dispatch<
    smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
    smithay::wayland::input_method::InputMethodUserData<Self>,
> for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
        request: smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::Request,
        data: &smithay::wayland::input_method::InputMethodUserData<Self>,
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        state.input_method_resource = Some(resource.clone());
        use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::Request;
        if state.session_is_locked()
            && let Request::GrabKeyboard { keyboard } = request
        {
            // GrabKeyboard carries a new object ID. Initialize an inert child before the fatal
            // policy error; returning with an uninitialized New makes wayland-server panic.
            data_init.init(keyboard, ());
            resource.post_error(0u32, "input-method keyboard grabs are disabled while locked");
            return;
        }
        <InputMethodManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
        data: &smithay::wayland::input_method::InputMethodUserData<Self>,
    ) {
        state.input_method_claimed = false;
        if state
            .input_method_client
            .as_ref()
            .is_some_and(|claimed| claimed.id() == client)
        {
            state.input_method_client = None;
            state.input_method_manager_resource = None;
        }
        if state.input_method_resource.as_ref() == Some(resource) {
            state.input_method_resource = None;
        }
        <InputMethodManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}
smithay::reexports::wayland_server::delegate_global_dispatch!(Astera: [
    smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1:
    smithay::wayland::virtual_keyboard::VirtualKeyboardManagerGlobalData
] => smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState);
impl smithay::reexports::wayland_server::Dispatch<
    smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    (),
> for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        request: smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::Request,
        data: &(),
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        let creates_keyboard = matches!(
            &request,
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::Request::CreateVirtualKeyboard { .. }
        );
        if creates_keyboard {
            if let Some((_, _, count)) = state
                .virtual_keyboard_clients
                .iter_mut()
                .find(|(known, _, _)| known.id() == client.id())
            {
                *count = count.saturating_add(1);
            } else {
                state
                    .virtual_keyboard_clients
                    .push((client.clone(), resource.clone(), 1));
            }
        }
        <VirtualKeyboardManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
        if creates_keyboard && state.session_is_locked() {
            resource.post_error(0u32, "virtual keyboards are unavailable while locked");
        }
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        data: &(),
    ) {
        <VirtualKeyboardManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

impl smithay::reexports::wayland_server::Dispatch<
    smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<Self>,
> for Astera
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        request: smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::Request,
        data: &smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<Self>,
        display: &DisplayHandle,
        data_init: &mut smithay::reexports::wayland_server::DataInit<'_, Self>,
    ) {
        if state.session_is_locked() && !matches!(request, smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::Request::Destroy) {
            // A fatal error makes the required reconnect explicit and prevents silently losing a
            // keymap or release, which would desynchronise client and compositor state.
            resource.post_error(0u32, "virtual keyboards must reconnect after session lock");
            return;
        }
        if matches!(
            request,
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::Request::Key { .. }
        ) {
            let events = state
                .idle_runtime
                .activity(state.idle_seat_key(), state.clock.now());
            state.send_idle_events(events);
        }
        <VirtualKeyboardManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, display, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        data: &smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<Self>,
    ) {
        <VirtualKeyboardManagerState as smithay::reexports::wayland_server::Dispatch<_, _, Self>>::destroyed(
            state, client.clone(), resource, data,
        );
        if let Some(index) = state
            .virtual_keyboard_clients
            .iter()
            .position(|(known, _, _)| known.id() == client)
        {
            let count = &mut state.virtual_keyboard_clients[index].2;
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.virtual_keyboard_clients.remove(index);
            }
        }
    }
}
delegate_relative_pointer!(Astera);
delegate_pointer_constraints!(Astera);
smithay::reexports::wayland_server::delegate_global_dispatch!(Astera: [
    smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1:
    smithay::wayland::session_lock::SessionLockManagerGlobalData
] => smithay::wayland::session_lock::SessionLockManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Astera: [
    smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1: ()
] => smithay::wayland::session_lock::SessionLockManagerState);
delegate_layer_shell!(Astera);
delegate_fractional_scale!(Astera);
delegate_viewporter!(Astera);
smithay::reexports::wayland_server::delegate_global_dispatch!(Astera: [
    smithay::reexports::wayland_server::protocol::wl_output::WlOutput:
    smithay::wayland::output::WlOutputData
] => smithay::wayland::output::OutputManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Astera: [
    smithay::reexports::wayland_server::protocol::wl_output::WlOutput:
    smithay::wayland::output::OutputUserData
] => smithay::wayland::output::OutputManagerState);
delegate_compositor!(Astera);
delegate_content_type!(Astera);
delegate_fifo!(Astera);
delegate_alpha_modifier!(Astera);
delegate_shm!(Astera);
delegate_single_pixel_buffer!(Astera);
delegate_seat!(Astera);
smithay::reexports::wayland_server::delegate_global_dispatch!(Astera: [
    smithay::reexports::wayland_server::protocol::wl_data_device_manager::WlDataDeviceManager: ()
] => smithay::wayland::selection::data_device::DataDeviceState);
smithay::reexports::wayland_server::delegate_dispatch!(Astera: [
    smithay::reexports::wayland_server::protocol::wl_data_device_manager::WlDataDeviceManager: ()
] => smithay::wayland::selection::data_device::DataDeviceState);
smithay::reexports::wayland_server::delegate_global_dispatch!(Astera: [
    smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1:
    smithay::wayland::selection::primary_selection::PrimaryDeviceManagerGlobalData
] => smithay::wayland::selection::primary_selection::PrimarySelectionState);
smithay::reexports::wayland_server::delegate_dispatch!(Astera: [
    smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1: ()
] => smithay::wayland::selection::primary_selection::PrimarySelectionState);
smithay::reexports::wayland_server::delegate_dispatch!(Astera: [
    smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1:
    smithay::wayland::selection::primary_selection::PrimarySourceUserData
] => smithay::wayland::selection::primary_selection::PrimarySelectionState);
delegate_dmabuf!(Astera);
delegate_drm_syncobj!(Astera);
smithay::delegate_security_context!(Astera);
delegate_presentation!(Astera);
delegate_ext_data_control!(Astera);
delegate_xdg_system_bell!(Astera);
smithay::reexports::wayland_server::delegate_global_dispatch!(Astera: [
    smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timing_manager_v1::WpCommitTimingManagerV1: bool
] => smithay::wayland::commit_timing::CommitTimingManagerState);
