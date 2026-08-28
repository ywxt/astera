use std::collections::{BTreeMap, BTreeSet};

use astera_core::{Desktop, OutputId, WorkspaceId, WorkspaceTransaction};
use smithay::{
    output::Output as SmithayOutput,
    reexports::{
        wayland_protocols::ext::workspace::v1::server::{
            ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
            ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
            ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::{ClientId, GlobalId},
            protocol::wl_output::WlOutput,
        },
    },
};

use super::Astera;

#[derive(Debug)]
pub(super) struct ExtWorkspaceState {
    _global: GlobalId,
}

#[derive(Clone, Debug)]
pub(super) struct WorkspaceGroupData {
    manager: ExtWorkspaceManagerV1,
    output: OutputId,
}

#[derive(Clone, Debug)]
pub(super) struct WorkspaceData {
    manager: ExtWorkspaceManagerV1,
    workspace: WorkspaceId,
}

#[derive(Clone, Copy, Debug)]
enum PendingRequest {
    Activate(WorkspaceId),
    Assign(WorkspaceId, OutputId),
}

#[derive(Debug)]
struct GroupInstance {
    resource: ExtWorkspaceGroupHandleV1,
    output: OutputId,
    output_resources: Vec<WlOutput>,
    removed: bool,
}

#[derive(Debug)]
struct WorkspaceInstance {
    resource: ExtWorkspaceHandleV1,
    workspace: WorkspaceId,
    output: OutputId,
    index: usize,
    name: String,
    active: bool,
    urgent: bool,
    announced_group: bool,
    removed: bool,
}

#[derive(Debug)]
pub(super) struct WorkspaceManagerInstance {
    manager: ExtWorkspaceManagerV1,
    groups: BTreeMap<OutputId, GroupInstance>,
    workspaces: BTreeMap<WorkspaceId, WorkspaceInstance>,
    pending: Vec<PendingRequest>,
    stopped: bool,
}

impl ExtWorkspaceState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<Astera, ExtWorkspaceManagerV1, _>(1, ()),
        }
    }
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for Astera {
    fn bind(
        state: &mut Self,
        display: &DisplayHandle,
        client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        let mut instance = WorkspaceManagerInstance {
            manager,
            groups: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            pending: Vec::new(),
            stopped: false,
        };
        instance.sync(state, display, client);
        state.workspace_managers.push(instance);
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_workspace_manager_v1::Request::Commit => {
                state.commit_workspace_requests(manager);
            }
            ext_workspace_manager_v1::Request::Stop => {
                if let Some(instance) = state
                    .workspace_managers
                    .iter_mut()
                    .find(|instance| instance.manager == *manager)
                {
                    instance.stopped = true;
                    instance.pending.clear();
                    manager.finished();
                }
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &ExtWorkspaceManagerV1,
        _data: &(),
    ) {
        state
            .workspace_managers
            .retain(|instance| instance.manager != *resource);
    }
}

impl Dispatch<ExtWorkspaceGroupHandleV1, WorkspaceGroupData> for Astera {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtWorkspaceGroupHandleV1,
        request: ext_workspace_group_handle_v1::Request,
        _data: &WorkspaceGroupData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // Astera does not advertise CREATE_WORKSPACE, so this request is intentionally ignored.
            ext_workspace_group_handle_v1::Request::CreateWorkspace { .. }
            | ext_workspace_group_handle_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, WorkspaceData> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        data: &WorkspaceData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Some(instance) = state
            .workspace_managers
            .iter_mut()
            .find(|instance| instance.manager == data.manager)
        else {
            return;
        };
        if instance.stopped
            || instance
                .workspaces
                .get(&data.workspace)
                .is_none_or(|workspace| workspace.removed)
        {
            return;
        }
        match request {
            ext_workspace_handle_v1::Request::Activate => instance
                .pending
                .push(PendingRequest::Activate(data.workspace)),
            ext_workspace_handle_v1::Request::Assign { workspace_group } => {
                let Some(group) = workspace_group.data::<WorkspaceGroupData>() else {
                    return;
                };
                if group.manager == data.manager {
                    instance
                        .pending
                        .push(PendingRequest::Assign(data.workspace, group.output));
                }
            }
            // These capabilities are not advertised and are therefore policy no-ops.
            ext_workspace_handle_v1::Request::Deactivate
            | ext_workspace_handle_v1::Request::Remove
            | ext_workspace_handle_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl WorkspaceManagerInstance {
    fn sync(&mut self, state: &Astera, display: &DisplayHandle, client: &Client) {
        if self.stopped || !self.manager.is_alive() {
            return;
        }

        let urgent_workspaces = state
            .windows
            .iter()
            .filter(|window| window.mapped && window.urgent)
            .filter_map(|window| state.desktop.find_window(window.id).ok())
            .collect::<BTreeSet<_>>();
        let model_workspaces = state
            .desktop
            .outputs
            .iter()
            .flat_map(|(output, set)| {
                let urgent_workspaces = &urgent_workspaces;
                set.workspaces
                    .iter()
                    .enumerate()
                    .map(move |(index, workspace)| {
                        (
                            workspace.id,
                            *output,
                            index,
                            workspace
                                .name
                                .clone()
                                .unwrap_or_else(|| format!("Workspace {}", index + 1)),
                            set.active == index,
                            urgent_workspaces.contains(&workspace.id),
                        )
                    })
            })
            .collect::<Vec<_>>();
        let current_ids = model_workspaces
            .iter()
            .map(|(workspace, ..)| *workspace)
            .collect::<BTreeSet<_>>();

        for workspace in self.workspaces.values_mut() {
            if !workspace.removed && !current_ids.contains(&workspace.workspace) {
                if let Some(group) = self.groups.get(&workspace.output)
                    && !group.removed
                    && group.resource.is_alive()
                    && workspace.resource.is_alive()
                {
                    group.resource.workspace_leave(&workspace.resource);
                }
                if workspace.resource.is_alive() {
                    workspace.resource.removed();
                }
                workspace.removed = true;
            }
        }

        // Workspace objects are announced unassigned before group membership is sent.
        for (workspace, output, index, name, active, urgent) in &model_workspaces {
            if self.workspaces.contains_key(workspace) {
                continue;
            }
            let Ok(resource) = client.create_resource::<ExtWorkspaceHandleV1, _, Astera>(
                display,
                self.manager.version(),
                WorkspaceData {
                    manager: self.manager.clone(),
                    workspace: *workspace,
                },
            ) else {
                continue;
            };
            self.manager.workspace(&resource);
            resource.name(name.clone());
            resource.coordinates((*index as u32).to_ne_bytes().to_vec());
            resource.state(workspace_state(*active, *urgent));
            resource.capabilities(
                ext_workspace_handle_v1::WorkspaceCapabilities::Activate
                    | ext_workspace_handle_v1::WorkspaceCapabilities::Assign,
            );
            self.workspaces.insert(
                *workspace,
                WorkspaceInstance {
                    resource,
                    workspace: *workspace,
                    output: *output,
                    index: *index,
                    name: name.clone(),
                    active: *active,
                    urgent: *urgent,
                    announced_group: false,
                    removed: false,
                },
            );
        }

        for output in state.desktop.outputs.keys().copied() {
            if self.groups.contains_key(&output) {
                continue;
            }
            let Ok(resource) = client.create_resource::<ExtWorkspaceGroupHandleV1, _, Astera>(
                display,
                self.manager.version(),
                WorkspaceGroupData {
                    manager: self.manager.clone(),
                    output,
                },
            ) else {
                continue;
            };
            self.manager.workspace_group(&resource);
            resource.capabilities(ext_workspace_group_handle_v1::GroupCapabilities::empty());
            let output_resources = state
                .output_runtime
                .get(&output)
                .map(|runtime| runtime.wayland.client_outputs(client).collect::<Vec<_>>())
                .unwrap_or_default();
            for wl_output in &output_resources {
                resource.output_enter(wl_output);
            }
            self.groups.insert(
                output,
                GroupInstance {
                    resource,
                    output,
                    output_resources,
                    removed: false,
                },
            );
        }

        let current_outputs = state.desktop.outputs.keys().copied().collect::<Vec<_>>();
        for group in self.groups.values_mut() {
            if !group.removed && !current_outputs.contains(&group.output) {
                for workspace in self
                    .workspaces
                    .values()
                    .filter(|workspace| !workspace.removed && workspace.output == group.output)
                {
                    if group.resource.is_alive() && workspace.resource.is_alive() {
                        group.resource.workspace_leave(&workspace.resource);
                    }
                }
                if group.resource.is_alive() {
                    for output in &group.output_resources {
                        if output.is_alive() {
                            group.resource.output_leave(output);
                        }
                    }
                    group.resource.removed();
                }
                group.removed = true;
            }
        }

        for (workspace_id, output, index, name, active, urgent) in model_workspaces {
            let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
                continue;
            };
            if workspace.removed || !workspace.resource.is_alive() {
                continue;
            }
            if workspace.output != output {
                if workspace.announced_group
                    && let Some(old_group) = self.groups.get(&workspace.output)
                    && !old_group.removed
                    && old_group.resource.is_alive()
                {
                    old_group.resource.workspace_leave(&workspace.resource);
                }
                if let Some(new_group) = self.groups.get(&output)
                    && !new_group.removed
                    && new_group.resource.is_alive()
                {
                    new_group.resource.workspace_enter(&workspace.resource);
                }
                workspace.output = output;
                workspace.announced_group = true;
            } else if !workspace.announced_group
                && let Some(group) = self.groups.get(&output)
                && !group.removed
                && group.resource.is_alive()
            {
                group.resource.workspace_enter(&workspace.resource);
                workspace.announced_group = true;
            }
            if workspace.index != index {
                workspace
                    .resource
                    .coordinates((index as u32).to_ne_bytes().to_vec());
                workspace.index = index;
            }
            if workspace.name != name {
                workspace.resource.name(name.clone());
                workspace.name = name;
            }
            if workspace.urgent != urgent || workspace.active != active {
                workspace.resource.state(workspace_state(active, urgent));
                workspace.urgent = urgent;
                workspace.active = active;
            }
        }
        self.manager.done();
    }
}

impl Astera {
    pub(super) fn sync_workspace_protocol(&mut self) {
        let mut managers = std::mem::take(&mut self.workspace_managers);
        let display = self.display.clone();
        managers.retain_mut(|instance| {
            let Ok(client) = display.get_client(instance.manager.id()) else {
                return false;
            };
            instance.sync(self, &display, &client);
            instance.manager.is_alive() && !instance.stopped
        });
        self.workspace_managers = managers;
    }

    pub(super) fn workspace_output_bound(&mut self, output: &SmithayOutput, wl_output: WlOutput) {
        let Some(output_id) = self
            .output_runtime
            .iter()
            .find_map(|(id, runtime)| (runtime.wayland == *output).then_some(*id))
        else {
            return;
        };
        for instance in &mut self.workspace_managers {
            if instance.manager.id().same_client_as(&wl_output.id())
                && let Some(group) = instance.groups.get_mut(&output_id)
                && !group.removed
                && group.resource.is_alive()
            {
                group.resource.output_enter(&wl_output);
                group.output_resources.push(wl_output.clone());
                instance.manager.done();
            }
        }
    }

    fn commit_workspace_requests(&mut self, manager: &ExtWorkspaceManagerV1) {
        let Some(index) = self
            .workspace_managers
            .iter()
            .position(|instance| instance.manager == *manager && !instance.stopped)
        else {
            return;
        };
        let pending = std::mem::take(&mut self.workspace_managers[index].pending);
        let mut desktop: Desktop = self.desktop.clone();
        let mut activated_output = None;
        for request in pending {
            let result = match request {
                PendingRequest::Activate(workspace) => {
                    let Ok(location) = desktop.workspace_location(workspace) else {
                        return;
                    };
                    let Some(output) = location.output else {
                        return;
                    };
                    activated_output = Some(output);
                    desktop
                        .apply(WorkspaceTransaction::Focus { output, workspace })
                        .map(|_| ())
                }
                PendingRequest::Assign(workspace, target_output) => desktop
                    .apply(WorkspaceTransaction::Move {
                        workspace,
                        target_output,
                        target_index: None,
                        activate: false,
                    })
                    .map(|_| ()),
            };
            if result.is_err() {
                return;
            }
        }
        self.desktop = desktop;
        if let Some(output) = activated_output {
            self.active_output = output;
        }
        self.cancel_surface_bound_input();
        self.mark_public_dirty();
        self.configure_fullscreen_windows();
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
        self.handle_pointer_motion(self.pointer_location, 0);
        self.sync_workspace_protocol();
    }
}

fn workspace_state(active: bool, urgent: bool) -> ext_workspace_handle_v1::State {
    let mut state = ext_workspace_handle_v1::State::empty();
    state.set(ext_workspace_handle_v1::State::Active, active);
    state.set(ext_workspace_handle_v1::State::Urgent, urgent);
    state
}
