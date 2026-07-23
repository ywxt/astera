use super::*;

impl BufferHandler for Astera {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
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
        // Creating an xdg role is not mapping. The first non-null buffer maps the window and a
        // null-buffer commit unmaps it while preserving the role for a later remap.
        if let Some(index) = self
            .windows
            .iter()
            .position(|window| window.surface.wl_surface() == surface)
        {
            let has_buffer = with_renderer_surface_state(surface, |state| state.buffer().is_some())
                .unwrap_or(false);
            match (self.windows[index].mapped, has_buffer) {
                (false, true) => self.map_toplevel(index),
                (true, false) => self.unmap_toplevel(index),
                _ => {}
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
            let has_buffer = with_renderer_surface_state(surface, |state| state.buffer().is_some())
                .unwrap_or(false);
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
delegate_layer_shell!(Astera);
delegate_fractional_scale!(Astera);
delegate_viewporter!(Astera);
delegate_output!(Astera);
delegate_compositor!(Astera);
delegate_shm!(Astera);
delegate_seat!(Astera);
delegate_data_device!(Astera);
delegate_dmabuf!(Astera);
