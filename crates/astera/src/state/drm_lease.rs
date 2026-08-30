use std::collections::HashMap;

use smithay::{
    backend::drm::{DrmDevice, DrmNode},
    delegate_drm_lease,
    reexports::drm::control::{connector, crtc},
    wayland::drm_lease::{
        DrmLease, DrmLeaseBuilder, DrmLeaseHandler, DrmLeaseRequest, DrmLeaseState, LeaseRejected,
    },
};

use super::Astera;

#[derive(Debug)]
pub(super) struct DrmLeaseRuntime {
    pub(super) protocol: DrmLeaseState,
    device: DrmDevice,
    connectors: HashMap<connector::Handle, crtc::Handle>,
    active: Vec<DrmLease>,
}

impl Astera {
    pub(crate) fn register_drm_lease_device(
        &mut self,
        node: DrmNode,
        device: DrmDevice,
    ) -> Result<(), smithay::wayland::drm_lease::Error> {
        if self.drm_leases.contains_key(&node) {
            return Ok(());
        }
        let protocol = DrmLeaseState::new::<Self>(&self.display, &node)?;
        self.drm_leases.insert(
            node,
            DrmLeaseRuntime {
                protocol,
                device,
                connectors: HashMap::new(),
                active: Vec::new(),
            },
        );
        Ok(())
    }

    pub(crate) fn add_drm_lease_connector(
        &mut self,
        node: DrmNode,
        connector: connector::Handle,
        crtc: crtc::Handle,
        name: String,
        description: String,
    ) {
        let Some(runtime) = self.drm_leases.get_mut(&node) else {
            return;
        };
        runtime.connectors.insert(connector, crtc);
        runtime
            .protocol
            .add_connector::<Self>(connector, name, description);
    }

    pub(crate) fn remove_drm_lease_connector(
        &mut self,
        node: DrmNode,
        connector: connector::Handle,
    ) {
        let Some(runtime) = self.drm_leases.get_mut(&node) else {
            return;
        };
        runtime.protocol.withdraw_connector(connector);
        runtime.connectors.remove(&connector);
        runtime
            .active
            .retain(|lease| !lease.connectors().any(|leased| *leased == connector));
    }

    pub(crate) fn unregister_drm_lease_device(&mut self, node: DrmNode) {
        if let Some(mut runtime) = self.drm_leases.remove(&node) {
            runtime.protocol.disable_global::<Self>();
        }
    }

    pub(crate) fn suspend_drm_leases(&mut self) {
        for runtime in self.drm_leases.values_mut() {
            runtime.protocol.suspend();
            runtime.active.clear();
        }
    }

    pub(crate) fn resume_drm_leases(&mut self) {
        for runtime in self.drm_leases.values_mut() {
            runtime.protocol.resume::<Self>();
        }
    }
}

impl DrmLeaseHandler for Astera {
    fn drm_lease_state(&mut self, node: DrmNode) -> &mut DrmLeaseState {
        &mut self
            .drm_leases
            .get_mut(&node)
            .expect("DRM lease request references a registered device")
            .protocol
    }

    fn lease_request(
        &mut self,
        node: DrmNode,
        request: DrmLeaseRequest,
    ) -> Result<DrmLeaseBuilder, LeaseRejected> {
        let runtime = self
            .drm_leases
            .get_mut(&node)
            .ok_or_else(LeaseRejected::default)?;
        let mut builder = DrmLeaseBuilder::new(&runtime.device);
        for connector in request.connectors {
            let crtc = *runtime
                .connectors
                .get(&connector)
                .ok_or_else(LeaseRejected::default)?;
            let planes = runtime
                .device
                .planes(&crtc)
                .map_err(LeaseRejected::with_cause)?;
            let (plane, claim) = planes
                .primary
                .into_iter()
                .find_map(|plane| {
                    runtime
                        .device
                        .claim_plane(plane.handle, crtc)
                        .map(|claim| (plane.handle, claim))
                })
                .ok_or_else(LeaseRejected::default)?;
            builder.add_connector(connector);
            builder.add_crtc(crtc);
            builder.add_plane(plane, claim);
        }
        Ok(builder)
    }

    fn new_active_lease(&mut self, node: DrmNode, lease: DrmLease) {
        if let Some(runtime) = self.drm_leases.get_mut(&node) {
            runtime.active.push(lease);
        }
    }

    fn lease_destroyed(&mut self, node: DrmNode, lease_id: u32) {
        if let Some(runtime) = self.drm_leases.get_mut(&node) {
            runtime.active.retain(|lease| lease.id() != lease_id);
        }
    }
}

delegate_drm_lease!(Astera);
