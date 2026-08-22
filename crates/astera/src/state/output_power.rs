use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use astera_core::OutputId;
use smithay::reexports::{
    wayland_protocols_wlr::output_power_management::v1::server::{
        zwlr_output_power_manager_v1::{self, ZwlrOutputPowerManagerV1},
        zwlr_output_power_v1::{self, ZwlrOutputPowerV1},
    },
    wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
        protocol::wl_output::WlOutput,
    },
};

use super::Astera;

pub(super) struct OutputPowerGlobalData {
    pub(super) visible: Arc<AtomicBool>,
}

pub(super) struct OutputPowerData {
    output: Option<OutputId>,
    valid: AtomicBool,
}

impl Astera {
    pub(crate) fn enable_output_power_management(&self) {
        self.output_power_advertised.store(true, Ordering::Relaxed);
    }

    pub(crate) fn take_output_power_requests(&mut self) -> Vec<(OutputId, bool)> {
        std::mem::take(&mut self.pending_output_power)
            .into_iter()
            .collect()
    }

    pub(crate) fn output_is_powered(&self, output: OutputId) -> bool {
        self.output_power_modes
            .get(&output)
            .copied()
            .unwrap_or(true)
    }

    pub(crate) fn confirm_output_power(&mut self, output: OutputId, powered: bool) {
        self.output_power_modes.insert(output, powered);
        self.session_output_powered(output, powered);
        if let Some(control) = self.output_power_controls.get(&output) {
            control.mode(if powered {
                zwlr_output_power_v1::Mode::On
            } else {
                zwlr_output_power_v1::Mode::Off
            });
        }
        tracing::info!(?output, powered, "output power mode changed");
    }

    pub(crate) fn fail_output_power(&mut self, output: OutputId) {
        if let Some(control) = self.output_power_controls.remove(&output) {
            if let Some(data) = control.data::<OutputPowerData>() {
                data.valid.store(false, Ordering::Relaxed);
            }
            control.failed();
        }
        tracing::warn!(?output, "output power control failed");
    }

    pub(super) fn output_power_connected(&mut self, output: OutputId) {
        self.output_power_modes.insert(output, true);
    }

    pub(super) fn output_power_disconnected(&mut self, output: OutputId) {
        if let Some(control) = self.output_power_controls.remove(&output) {
            if let Some(data) = control.data::<OutputPowerData>() {
                data.valid.store(false, Ordering::Relaxed);
            }
            control.failed();
        }
        self.output_power_modes.remove(&output);
        self.pending_output_power
            .retain(|(candidate, _)| *candidate != output);
    }

    fn bind_output_power(
        &mut self,
        id: New<ZwlrOutputPowerV1>,
        requested: WlOutput,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let output = smithay::output::Output::from_resource(&requested).and_then(|requested| {
            self.output_runtime
                .iter()
                .find_map(|(id, runtime)| (runtime.wayland == requested).then_some(*id))
        });
        let accepted = output.filter(|output| !self.output_power_controls.contains_key(output));
        let control = data_init.init(
            id,
            OutputPowerData {
                output: accepted,
                valid: AtomicBool::new(accepted.is_some()),
            },
        );
        let Some(output) = accepted else {
            control.failed();
            return;
        };
        let powered = self
            .output_power_modes
            .get(&output)
            .copied()
            .unwrap_or(true);
        self.output_power_controls.insert(output, control.clone());
        control.mode(if powered {
            zwlr_output_power_v1::Mode::On
        } else {
            zwlr_output_power_v1::Mode::Off
        });
    }
}

impl GlobalDispatch<ZwlrOutputPowerManagerV1, OutputPowerGlobalData> for Astera {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputPowerManagerV1>,
        _global: &OutputPowerGlobalData,
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, global: &OutputPowerGlobalData) -> bool {
        // Astera's current trust boundary is the compositor user's private runtime directory:
        // Wayland and IPC clients running as that user are equally authorized to control the
        // session. A future security-context policy can narrow this filter without changing the
        // protocol state machine.
        let _ = client;
        global.visible.load(Ordering::Relaxed)
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrOutputPowerManagerV1,
        request: zwlr_output_power_manager_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_power_manager_v1::Request::GetOutputPower { id, output } => {
                state.bind_output_power(id, output, data_init);
            }
            zwlr_output_power_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, OutputPowerData> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        control: &ZwlrOutputPowerV1,
        request: zwlr_output_power_v1::Request,
        data: &OutputPowerData,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_power_v1::Request::SetMode { mode } => {
                if !data.valid.load(Ordering::Relaxed) {
                    return;
                }
                let WEnum::Value(mode) = mode else {
                    control.post_error(
                        zwlr_output_power_v1::Error::InvalidMode,
                        "unknown output power mode",
                    );
                    return;
                };
                let Some(output) = data.output.filter(|output| {
                    state.output_power_controls.get(output) == Some(control)
                        && state.output_runtime.contains_key(output)
                }) else {
                    control.failed();
                    return;
                };
                let powered = mode == zwlr_output_power_v1::Mode::On;
                state
                    .pending_output_power
                    .retain(|(candidate, _)| *candidate != output);
                state.pending_output_power.push_back((output, powered));
                tracing::debug!(?output, powered, "output power mode requested");
            }
            zwlr_output_power_v1::Request::Destroy => {
                if let Some(output) = data.output
                    && state.output_power_controls.get(&output) == Some(control)
                {
                    state.output_power_controls.remove(&output);
                }
                data.valid.store(false, Ordering::Relaxed);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        control: &ZwlrOutputPowerV1,
        data: &OutputPowerData,
    ) {
        if let Some(output) = data.output
            && state.output_power_controls.get(&output) == Some(control)
        {
            state.output_power_controls.remove(&output);
        }
    }
}
