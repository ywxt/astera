use smithay::{
    backend::input::{
        ButtonState, Device, DeviceCapability, InputBackend, ProximityState, TabletToolAxisEvent,
        TabletToolButtonEvent, TabletToolEvent, TabletToolProximityEvent, TabletToolTipEvent,
        TabletToolTipState,
    },
    reexports::wayland_server::Resource,
    wayland::tablet_manager::{TabletDescriptor, TabletSeatTrait},
};

use super::{Astera, saturating_i32};

type TabletFocus = Option<(
    smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    smithay::utils::Point<f64, smithay::utils::Logical>,
    Option<astera_core::WindowId>,
)>;
type TabletProtocolFocus = Option<(
    smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    smithay::utils::Point<f64, smithay::utils::Logical>,
)>;

pub(super) struct TabletToolRuntime {
    pub(super) handle: smithay::wayland::tablet_manager::TabletToolHandle,
    focus: TabletFocus,
    tip_down: bool,
    pressed_buttons: std::collections::HashSet<u32>,
    pub(super) cursor_image: smithay::input::pointer::CursorImageStatus,
    pub(super) cursor_location: Option<(
        astera_core::OutputId,
        smithay::utils::Point<f64, smithay::utils::Logical>,
    )>,
}

impl TabletToolRuntime {
    fn is_grabbed(&self) -> bool {
        self.tip_down || !self.pressed_buttons.is_empty()
    }
}

fn routed_focus<T: Clone>(grabbed: bool, current: &Option<T>, hit: Option<T>) -> Option<T> {
    if grabbed { current.clone() } else { hit }
}

struct TabletContext {
    tablet: smithay::wayland::tablet_manager::TabletHandle,
    tool: smithay::wayland::tablet_manager::TabletToolHandle,
    output: astera_core::OutputId,
    location: smithay::utils::Point<f64, smithay::utils::Logical>,
    focus: TabletProtocolFocus,
    window: Option<astera_core::WindowId>,
}

impl Astera {
    pub(super) fn tablet_device_added<D: Device>(&mut self, device: &D) {
        if !device.has_capability(DeviceCapability::TabletTool) {
            return;
        }
        let descriptor = TabletDescriptor::from(device);
        let handle = self
            .seat
            .tablet_seat()
            .add_tablet::<Self>(&self.display, &descriptor);
        self.tablets.insert(device.id(), (descriptor, handle));
    }

    pub(super) fn tablet_device_removed<D: Device>(&mut self, device: &D) {
        let id = device.id();
        if self.touch_slots.keys().any(|(device, _)| device == &id) {
            self.cancel_touch_sequences();
        }
        self.touch_device_outputs.remove(&id);
        let Some((descriptor, _)) = self.tablets.remove(&id) else {
            return;
        };
        self.seat.tablet_seat().remove_tablet(&descriptor);
        // A tool may roam between tablets, so retain tool protocol objects but end any focus held
        // by the removed physical device.
        self.cancel_tablet_focus(0);
    }

    fn tablet_context<B: InputBackend, E: TabletToolEvent<B>>(
        &mut self,
        event: &E,
    ) -> Option<TabletContext> {
        let device = event.device();
        if !self.tablets.contains_key(&device.id()) {
            self.tablet_device_added(&device);
        }
        let tablet = self.tablets.get(&device.id())?.1.clone();
        let descriptor = event.tool();
        let tool = if let Some(tool) = self.tablet_tools.get(&descriptor) {
            tool.handle.clone()
        } else {
            let seat = self.seat.tablet_seat();
            let display = self.display.clone();
            let tool = seat.add_tool(self, &display, &descriptor);
            self.tablet_tools.insert(
                descriptor.clone(),
                TabletToolRuntime {
                    handle: tool.clone(),
                    focus: None,
                    tip_down: false,
                    pressed_buttons: Default::default(),
                    cursor_image: smithay::input::pointer::CursorImageStatus::default_named(),
                    cursor_location: None,
                },
            );
            tool
        };
        let output = self.touch_output_for_device(&device.id())?;
        let size = self.desktop.outputs.get(&output)?.output.logical_size;
        let location = event
            .position_transformed((saturating_i32(size.width), saturating_i32(size.height)).into());
        let previous_output = self.active_output;
        self.active_output = output;
        let hit = self.surface_under(location);
        self.active_output = previous_output;
        let hit_focus = hit;
        let runtime = self.tablet_tools.get_mut(&descriptor)?;
        let changed_client = !runtime.is_grabbed()
            && match (&runtime.focus, &hit_focus) {
                (Some((old, _, _)), Some((new, _, _))) => !old.id().same_client_as(&new.id()),
                (Some(_), None) | (None, Some(_)) => true,
                (None, None) => false,
            };
        if changed_client {
            runtime.cursor_image = smithay::input::pointer::CursorImageStatus::default_named();
        }
        let changed_output = runtime
            .cursor_location
            .is_some_and(|(previous, _)| previous != output);
        runtime.cursor_location = Some((output, location));
        let changed_owner = self.active_tablet_cursor.as_ref() != Some(&descriptor);
        self.active_tablet_cursor = Some(descriptor.clone());
        // tablet-v2 has an implicit grab while the tip or a tool button is down. Keep routing to
        // the original surface until the complete physical sequence has been released.
        let grabbed = runtime.is_grabbed();
        let focus = routed_focus(grabbed, &runtime.focus, hit_focus.clone());
        if !grabbed {
            runtime.focus = hit_focus.clone();
        }
        let window = focus.as_ref().and_then(|(_, _, window)| *window);
        let focus = focus.map(|(surface, origin, _)| (surface, origin));
        self.mark_render_dirty();
        if changed_client || changed_owner || changed_output {
            self.refresh_visible_scales();
        }
        Some(TabletContext {
            tablet,
            tool,
            output,
            location,
            focus,
            window,
        })
    }

    fn apply_tablet_axes<B: InputBackend, E: TabletToolEvent<B>>(
        tool: &smithay::wayland::tablet_manager::TabletToolHandle,
        event: &E,
    ) {
        if event.pressure_has_changed() {
            tool.pressure(event.pressure());
        }
        if event.distance_has_changed() {
            tool.distance(event.distance());
        }
        if event.tilt_has_changed() {
            tool.tilt(event.tilt());
        }
        if event.rotation_has_changed() {
            tool.rotation(event.rotation());
        }
        if event.slider_has_changed() {
            tool.slider_position(event.slider_position());
        }
        if event.wheel_has_changed() {
            tool.wheel(event.wheel_delta(), event.wheel_delta_discrete());
        }
    }

    pub(super) fn handle_tablet_proximity<B: InputBackend, E: TabletToolProximityEvent<B>>(
        &mut self,
        event: E,
    ) {
        if event.state() == ProximityState::Out {
            let serial = self.next_serial();
            let descriptor = event.tool();
            if let Some(runtime) = self.tablet_tools.get_mut(&descriptor) {
                for button in runtime.pressed_buttons.drain() {
                    runtime
                        .handle
                        .button(button, ButtonState::Released, serial, event.time_msec());
                }
                runtime.tip_down = false;
                runtime.focus = None;
                runtime.cursor_location = None;
                runtime.cursor_image = smithay::input::pointer::CursorImageStatus::default_named();
                runtime.handle.proximity_out(event.time_msec());
            }
            if self.active_tablet_cursor.as_ref() == Some(&descriptor) {
                self.active_tablet_cursor = None;
            }
            self.mark_render_dirty();
            self.refresh_visible_scales();
            return;
        }
        let Some(TabletContext {
            tablet,
            tool,
            location,
            focus,
            ..
        }) = self.tablet_context(&event)
        else {
            return;
        };
        if let Some(focus) = focus {
            let serial = self.next_serial();
            tool.proximity_in(location, focus.clone(), &tablet, serial, event.time_msec());
            // Smithay emits the protocol-required initial motion from proximity_in, then this
            // second frame flushes pressure/tilt/etc. carried by the libinput proximity event.
            Self::apply_tablet_axes(&tool, &event);
            tool.motion(location, Some(focus), &tablet, serial, event.time_msec());
        }
    }

    pub(super) fn handle_tablet_axis<B: InputBackend, E: TabletToolAxisEvent<B>>(
        &mut self,
        event: E,
    ) {
        let Some(TabletContext {
            tablet,
            tool,
            location,
            focus,
            ..
        }) = self.tablet_context(&event)
        else {
            return;
        };
        Self::apply_tablet_axes(&tool, &event);
        let serial = self.next_serial();
        tool.motion(location, focus, &tablet, serial, event.time_msec());
    }

    pub(super) fn handle_tablet_tip<B: InputBackend, E: TabletToolTipEvent<B>>(
        &mut self,
        event: E,
    ) {
        let Some(TabletContext {
            tablet,
            tool,
            output,
            location,
            focus,
            window,
        }) = self.tablet_context(&event)
        else {
            return;
        };
        Self::apply_tablet_axes(&tool, &event);
        let serial = self.next_serial();
        tool.motion(location, focus, &tablet, serial, event.time_msec());
        if event.tip_state() == TabletToolTipState::Down {
            self.active_output = output;
            if let Some(surface) = self
                .tablet_tools
                .get(&event.tool())
                .and_then(|runtime| runtime.focus.as_ref())
                .map(|(surface, _, _)| surface.clone())
            {
                self.focus_interaction_target(&surface, window);
                if let Some(client) = surface.client() {
                    self.activation_tracker
                        .remember(serial, client.id(), self.clock.now());
                }
            } else {
                self.sync_keyboard_focus();
            }
        }
        let runtime = self
            .tablet_tools
            .get_mut(&event.tool())
            .expect("tool was created");
        match event.tip_state() {
            TabletToolTipState::Down => {
                runtime.tip_down = true;
                tool.tip_down(serial, event.time_msec());
            }
            TabletToolTipState::Up => {
                tool.tip_up(event.time_msec());
                runtime.tip_down = false;
            }
        }
    }

    pub(super) fn handle_tablet_button<B: InputBackend, E: TabletToolButtonEvent<B>>(
        &mut self,
        event: E,
    ) {
        let Some(TabletContext {
            tablet,
            tool,
            output,
            location,
            focus,
            window,
        }) = self.tablet_context(&event)
        else {
            return;
        };
        let serial = self.next_serial();
        Self::apply_tablet_axes(&tool, &event);
        tool.motion(location, focus, &tablet, serial, event.time_msec());
        tool.button(
            event.button(),
            event.button_state(),
            serial,
            event.time_msec(),
        );
        if event.button_state() == ButtonState::Pressed {
            self.active_output = output;
            if let Some(surface) = self
                .tablet_tools
                .get(&event.tool())
                .and_then(|runtime| runtime.focus.as_ref())
                .map(|(surface, _, _)| surface.clone())
            {
                self.focus_interaction_target(&surface, window);
                if let Some(client) = surface.client() {
                    self.activation_tracker
                        .remember(serial, client.id(), self.clock.now());
                }
            } else {
                self.sync_keyboard_focus();
            }
        }
        let runtime = self
            .tablet_tools
            .get_mut(&event.tool())
            .expect("tool was created");
        match event.button_state() {
            ButtonState::Pressed => {
                runtime.pressed_buttons.insert(event.button());
            }
            ButtonState::Released => {
                runtime.pressed_buttons.remove(&event.button());
            }
        }
    }

    pub(super) fn cancel_tablet_focus(&mut self, time: u32) {
        let serial = self.next_serial();
        for runtime in self.tablet_tools.values_mut() {
            for button in runtime.pressed_buttons.drain() {
                runtime
                    .handle
                    .button(button, ButtonState::Released, serial, time);
            }
            runtime.tip_down = false;
            runtime.focus = None;
            runtime.cursor_location = None;
            runtime.cursor_image = smithay::input::pointer::CursorImageStatus::default_named();
            runtime.handle.proximity_out(time);
        }
        self.active_tablet_cursor = None;
        self.mark_render_dirty();
        self.refresh_visible_scales();
    }
}

#[cfg(test)]
mod tests {
    use super::routed_focus;

    #[test]
    fn implicit_grab_keeps_recipient_until_every_down_state_is_released() {
        let original = Some("canvas-a");
        assert_eq!(routed_focus(true, &original, Some("canvas-b")), original);
        assert_eq!(
            routed_focus(false, &original, Some("canvas-b")),
            Some("canvas-b")
        );
    }
}
