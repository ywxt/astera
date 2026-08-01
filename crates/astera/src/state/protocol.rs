use super::*;

impl BufferHandler for Astera {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
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
        if let Ok(workspace) = self.desktop.focus_window(window) {
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
        self.pending_dmabufs.push((dmabuf, notifier));
    }
}

impl OutputHandler for Astera {}

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

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popup_manager.commit(surface);
        let committed_buffer =
            with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false);
        // Buffer attachment, damage and subsurface state are visual changes even
        // when the public window metadata remains identical. A role's initial
        // null-buffer commit is protocol setup, however, and must not consume a
        // host frame before the surface maps.
        if committed_buffer {
            self.mark_render_dirty();
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
            if let Some(mapped) = self.layers.iter().find(|mapped| mapped.surface == layer)
                && let Some(runtime) = self.output_runtime.get(&mapped.output)
            {
                layer_map_for_output(&runtime.wayland).arrange();
            }
            let has_buffer = committed_buffer;
            if let Some(mapped) = self
                .layers
                .iter_mut()
                .find(|mapped| mapped.surface.wl_surface() == surface)
            {
                mapped.mapped = has_buffer;
                mapped.layer = layer.layer();
            }
            self.configure_layer_surface(&layer);
            self.refresh_visible_scales();
            self.sync_keyboard_focus();
            self.mark_public_dirty();
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
        // A missing wl_output means the compositor-selected active output, as required by the
        // layer-shell protocol; placement remains viewport-local to that output.
        let output = output
            .as_ref()
            .and_then(|requested| {
                self.output_runtime
                    .iter()
                    .find_map(|(id, runtime)| runtime.wayland.owns(requested).then_some(*id))
            })
            .unwrap_or(self.active_output);
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
        if let Err(error) = self.popup_manager.track_popup(popup.clone().into()) {
            tracing::warn!(%error, "could not track layer popup");
            return;
        }
        if let Err(error) = popup.send_configure() {
            tracing::warn!(%error, "could not configure layer popup");
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        if let Some(mapped) = self
            .layers
            .iter()
            .find(|mapped| mapped.surface.layer_surface() == &surface)
            && let Some(runtime) = self.output_runtime.get(&mapped.output)
        {
            layer_map_for_output(&runtime.wayland).unmap_layer(&mapped.surface);
        }
        self.layers
            .retain(|mapped| mapped.surface.layer_surface() != &surface);
        self.mark_public_dirty();
        tracing::debug!("layer surface destroyed");
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
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
        self.windows.push(MappedWindow {
            id,
            surface,
            mapped: false,
            urgent: false,
        });
        tracing::debug!(window = ?id, "toplevel role created");
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = positioner.get_geometry();
        });
        if let Err(error) = self.popup_manager.track_popup(surface.clone().into()) {
            tracing::warn!(%error, "could not track xdg popup");
            return;
        }
        if let Err(error) = surface.send_configure() {
            tracing::warn!(%error, "could not configure xdg popup");
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
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
        let Some(window) = self
            .windows
            .iter()
            .find(|window| window.mapped && window.surface == surface)
            .map(|window| window.id)
        else {
            return;
        };
        let Ok(mode) = self.toggle_fullscreen_mode(window) else {
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

    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        let popup = PopupKind::from(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let seat = self.seat.clone();
        let Ok(grab) = self
            .popup_manager
            .grab_popup::<Self>(root, popup, &seat, serial)
        else {
            return;
        };
        let keyboard = self.keyboard.clone();
        keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        let pointer = self.pointer.clone();
        pointer.set_grab(
            self,
            PopupPointerGrab::new(&grab),
            serial,
            PointerFocusMode::Keep,
        );
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(index) = self
            .windows
            .iter()
            .position(|window| window.surface == surface)
        else {
            return;
        };
        let window = self.windows.remove(index);
        if window.mapped
            && let Ok(workspace) = self.desktop.find_window(window.id)
        {
            let _ = self
                .desktop
                .apply_window(workspace, WindowTransaction::Remove { id: window.id });
        }
        if self.drag.is_some_and(|drag| drag.window == window.id) {
            self.drag = None;
        }
        tracing::info!(window = ?window.id, "toplevel role destroyed");
        self.mark_public_dirty();
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = positioner.get_geometry();
        });
        surface.send_repositioned(token);
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

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
}

impl SelectionHandler for Astera {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Astera {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for Astera {}

impl ServerDndGrabHandler for Astera {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

delegate_xdg_shell!(Astera);
delegate_xdg_decoration!(Astera);
delegate_xdg_activation!(Astera);
delegate_layer_shell!(Astera);
delegate_fractional_scale!(Astera);
delegate_viewporter!(Astera);
delegate_output!(Astera);
delegate_compositor!(Astera);
delegate_shm!(Astera);
delegate_seat!(Astera);
delegate_data_device!(Astera);
delegate_dmabuf!(Astera);
