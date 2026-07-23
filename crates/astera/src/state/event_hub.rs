use std::collections::BTreeMap;

use astera_ipc::wire::v1::{
    CameraSnapshot, DesktopSnapshot, Event, EventEnvelope, LayerSnapshot, OutputId, OutputSnapshot,
    WindowId, WindowSnapshot, WorkspaceId, WorkspaceSnapshot,
};

/// Owns the public state revision and computes the canonical event stream at tick boundaries.
///
/// The hub is intentionally independent of socket subscribers: publishing a state transition
/// always advances the sequence, so reconnecting observers see the same revision history whether
/// or not anybody happened to be listening at the time.
#[derive(Default)]
pub(super) struct EventHub {
    sequence: u64,
    previous: Option<DesktopSnapshot>,
    explicit: Vec<Event>,
    latest: Vec<EventEnvelope>,
    sequence_overflowed: bool,
}

impl EventHub {
    pub(super) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) fn config_loaded(&mut self, generation: u64, failed: bool, error: Option<String>) {
        self.explicit.push(Event::ConfigLoaded {
            generation,
            failed,
            error,
        });
    }

    /// A sequence reset invalidates every existing stream; the broadcaster consumes this flag and
    /// disconnects subscribers before routing events from the new sequence epoch.
    pub(super) fn take_sequence_overflow(&mut self) -> bool {
        std::mem::take(&mut self.sequence_overflowed)
    }

    pub(super) fn publish(&mut self, snapshot: DesktopSnapshot) -> &[EventEnvelope] {
        let events = match self.previous.as_ref() {
            Some(previous) => diff(previous, &snapshot),
            None => diff(&DesktopSnapshot::default(), &snapshot),
        };
        self.latest.clear();
        for event in events.into_iter().chain(self.explicit.drain(..)) {
            if self.sequence == u64::MAX {
                self.sequence = 0;
                self.sequence_overflowed = true;
            }
            self.sequence += 1;
            let envelope = EventEnvelope {
                sequence: self.sequence,
                event,
            };
            self.latest.push(envelope);
        }
        self.previous = Some(snapshot);
        &self.latest
    }

    pub(super) fn clean_tick(&mut self) -> &[EventEnvelope] {
        self.latest.clear();
        &self.latest
    }
}

fn by_output(snapshot: &DesktopSnapshot) -> BTreeMap<OutputId, &OutputSnapshot> {
    snapshot
        .outputs
        .iter()
        .map(|item| (item.id, item))
        .collect()
}

fn by_workspace(snapshot: &DesktopSnapshot) -> BTreeMap<WorkspaceId, &WorkspaceSnapshot> {
    snapshot
        .workspaces
        .iter()
        .map(|item| (item.id, item))
        .collect()
}

fn by_layer(snapshot: &DesktopSnapshot) -> BTreeMap<u64, &LayerSnapshot> {
    snapshot.layers.iter().map(|item| (item.id, item)).collect()
}

fn by_window(snapshot: &DesktopSnapshot) -> BTreeMap<WindowId, &WindowSnapshot> {
    snapshot
        .windows
        .iter()
        .map(|item| (item.id, item))
        .collect()
}

fn by_camera(snapshot: &DesktopSnapshot) -> BTreeMap<WorkspaceId, &CameraSnapshot> {
    snapshot
        .cameras
        .iter()
        .map(|item| (item.workspace, item))
        .collect()
}

/// Dependency order is stable and deliberate: children close before parents, parents open before
/// children, structural changes precede activation/focus, and explicit config notifications end
/// the tick after the state they describe has become observable.
fn diff(old: &DesktopSnapshot, new: &DesktopSnapshot) -> Vec<Event> {
    let old_outputs = by_output(old);
    let new_outputs = by_output(new);
    let old_workspaces = by_workspace(old);
    let new_workspaces = by_workspace(new);
    let old_layers = by_layer(old);
    let new_layers = by_layer(new);
    let old_windows = by_window(old);
    let new_windows = by_window(new);
    let old_cameras = by_camera(old);
    let new_cameras = by_camera(new);
    let mut events = Vec::new();

    for id in old_windows
        .keys()
        .filter(|id| !new_windows.contains_key(id))
    {
        events.push(Event::WindowClosed { window: *id });
    }
    for id in old_workspaces
        .keys()
        .filter(|id| !new_workspaces.contains_key(id))
    {
        events.push(Event::WorkspaceClosed { workspace: *id });
    }
    for id in old_layers.keys().filter(|id| !new_layers.contains_key(id)) {
        events.push(Event::LayerClosed { layer: *id });
    }
    for id in old_outputs
        .keys()
        .filter(|id| !new_outputs.contains_key(id))
    {
        events.push(Event::OutputClosed { output: *id });
    }

    for (id, output) in &new_outputs {
        if !old_outputs.contains_key(id) {
            events.push(Event::OutputOpened {
                output: (*output).clone(),
            });
        }
    }
    for (id, layer) in &new_layers {
        if !old_layers.contains_key(id) {
            events.push(Event::LayerOpened {
                layer: (*layer).clone(),
            });
        }
    }
    for (id, workspace) in &new_workspaces {
        if !old_workspaces.contains_key(id) {
            events.push(Event::WorkspaceOpened {
                workspace: (*workspace).clone(),
            });
        }
    }
    for (id, window) in &new_windows {
        if !old_windows.contains_key(id) {
            events.push(Event::WindowOpened {
                window: (*window).clone(),
            });
        }
    }

    for (id, output) in &new_outputs {
        if old_outputs
            .get(id)
            .is_some_and(|old| output_structural_changed(old, output))
        {
            events.push(Event::OutputChanged {
                output: (*output).clone(),
            });
        }
    }
    for (id, workspace) in &new_workspaces {
        if old_workspaces
            .get(id)
            .is_some_and(|old| workspace_structural_changed(old, workspace))
        {
            events.push(Event::WorkspaceChanged {
                workspace: (*workspace).clone(),
            });
        }
    }
    for (id, layer) in &new_layers {
        if old_layers.get(id).is_some_and(|old| *old != *layer) {
            events.push(Event::LayerChanged {
                layer: (*layer).clone(),
            });
        }
    }
    for (id, window) in &new_windows {
        let Some(old_window) = old_windows.get(id) else {
            continue;
        };
        let ordinary_changed = window_ordinary_changed(old_window, window);
        if ordinary_changed {
            events.push(Event::WindowChanged {
                window: (*window).clone(),
            });
        } else if old_window.placement != window.placement {
            events.push(Event::PlacementChanged {
                window: *id,
                placement: window.placement.clone(),
            });
        }
    }
    for (id, camera) in &new_cameras {
        if old_cameras.get(id).copied() != Some(*camera) {
            events.push(Event::CameraChanged {
                camera: (*camera).clone(),
            });
        }
    }

    for (id, output) in &new_outputs {
        let workspace_changed = old_outputs
            .get(id)
            .is_none_or(|old| old.active_workspace != output.active_workspace);
        let focus_changed = (old.active_output == Some(*id)) != (new.active_output == Some(*id));
        if workspace_changed || focus_changed {
            events.push(Event::WorkspaceActivated {
                output: *id,
                workspace: output.active_workspace,
                focused: new.active_output == Some(*id),
            });
        }
    }
    for (id, workspace) in &new_workspaces {
        if old_workspaces
            .get(id)
            .is_none_or(|old| old.active_window != workspace.active_window)
        {
            events.push(Event::WorkspaceActiveWindowChanged {
                workspace: *id,
                window: workspace.active_window,
            });
        }
    }
    if old.focused_window != new.focused_window {
        events.push(Event::WindowFocusChanged {
            id: new.focused_window,
        });
    }
    if old.active_output != new.active_output {
        events.push(Event::ActiveOutputChanged {
            output: new.active_output,
        });
    }
    if old.primary_output != new.primary_output {
        events.push(Event::PrimaryOutputChanged {
            output: new.primary_output,
        });
    }
    events
}

fn output_structural_changed(old: &OutputSnapshot, new: &OutputSnapshot) -> bool {
    let mut old = old.clone();
    let mut new = new.clone();
    old.active_workspace = WorkspaceId(0);
    new.active_workspace = WorkspaceId(0);
    old != new
}

fn workspace_structural_changed(old: &WorkspaceSnapshot, new: &WorkspaceSnapshot) -> bool {
    let mut old = old.clone();
    let mut new = new.clone();
    old.active_window = None;
    new.active_window = None;
    old != new
}

fn window_ordinary_changed(old: &WindowSnapshot, new: &WindowSnapshot) -> bool {
    old.id != new.id
        || old.workspace != new.workspace
        || old.mode != new.mode
        || old.metadata != new.metadata
}

#[cfg(test)]
mod tests {
    use super::*;
    use astera_ipc::wire::v1::{
        FullscreenRestore, Layer, OutputTransform, Rect, Scale120, Size, WindowMetadata,
        WindowMode, WindowPlacement,
    };

    fn output(id: u32, workspace: u32) -> OutputSnapshot {
        OutputSnapshot {
            id: OutputId(id),
            stable_key: format!("output-{id}"),
            active_workspace: WorkspaceId(workspace),
            workspaces: vec![WorkspaceId(workspace)],
            physical_size: Size::new(100, 100),
            logical_size: Size::new(100, 100),
            native_scale: Scale120(120),
            transform: OutputTransform::Normal,
            viewport: Rect::new(0, 0, 100, 100),
            usable_area: Rect::new(0, 0, 100, 100),
        }
    }

    fn workspace(id: u32, output: u32) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            id: WorkspaceId(id),
            name: None,
            original_output: None,
            output: Some(OutputId(output)),
            local_index: Some(1),
            active_window: None,
            tiled_count: 0,
            floating_count: 0,
            fullscreen: None,
        }
    }

    #[test]
    fn zero_subscribers_still_advance_and_dependency_order_is_stable() {
        let mut hub = EventHub::default();
        let mut snapshot = DesktopSnapshot {
            active_output: Some(OutputId(2)),
            ..DesktopSnapshot::default()
        };
        snapshot.outputs.push(output(2, 7));
        snapshot.layers.push(LayerSnapshot {
            id: 3,
            output: OutputId(2),
            namespace: "panel".into(),
            layer: Layer::Top,
            anchor: astera_ipc::wire::v1::Anchor::default(),
            exclusive_zone: 0,
            exclusive_contribution: Default::default(),
            keyboard_interactivity: astera_ipc::wire::v1::KeyboardInteractivity::None,
            geometry: Rect::new(0, 0, 100, 10),
        });
        snapshot.workspaces.push(workspace(7, 2));
        snapshot.cameras.push(CameraSnapshot {
            workspace: WorkspaceId(7),
            center: astera_ipc::wire::v1::Point::default(),
            policy: astera_ipc::wire::v1::CameraPolicy::Centered,
        });
        let published = hub.publish(snapshot);
        assert!(matches!(published[0].event, Event::OutputOpened { .. }));
        assert!(matches!(published[1].event, Event::LayerOpened { .. }));
        assert!(matches!(published[2].event, Event::WorkspaceOpened { .. }));
        assert!(matches!(published[3].event, Event::CameraChanged { .. }));
        assert!(matches!(
            published[4].event,
            Event::WorkspaceActivated { .. }
        ));
        assert!(matches!(
            published[5].event,
            Event::WorkspaceActiveWindowChanged { .. }
        ));
        assert!(matches!(
            published[6].event,
            Event::ActiveOutputChanged { .. }
        ));
        assert_eq!(hub.sequence(), 7);
    }

    #[test]
    fn explicit_config_event_is_published_even_without_structural_diff() {
        let mut hub = EventHub::default();
        hub.publish(DesktopSnapshot::default());
        hub.config_loaded(1, false, None);
        let events = hub.publish(DesktopSnapshot::default());
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].event, Event::ConfigLoaded { .. }));
    }

    fn window(placement: WindowPlacement, visible_geometry: Option<Rect>) -> WindowSnapshot {
        WindowSnapshot {
            id: WindowId(9),
            workspace: WorkspaceId(7),
            mode: WindowMode::Tiled,
            metadata: WindowMetadata::default(),
            placement,
            visible_geometry,
        }
    }

    #[test]
    fn placement_changes_are_folded_and_derived_geometry_is_ignored() {
        let original = window(
            WindowPlacement::Tiled {
                world_geometry: Rect::new(0, 0, 20, 20),
            },
            Some(Rect::new(0, 0, 20, 20)),
        );
        let mut snapshot = DesktopSnapshot {
            windows: vec![original.clone()],
            ..DesktopSnapshot::default()
        };
        let mut hub = EventHub::default();
        hub.publish(snapshot.clone());

        snapshot.windows[0].visible_geometry = Some(Rect::new(50, 50, 20, 20));
        assert!(hub.publish(snapshot.clone()).is_empty());

        snapshot.windows[0].placement = WindowPlacement::Tiled {
            world_geometry: Rect::new(5, 5, 20, 20),
        };
        let events = hub.publish(snapshot.clone());
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].event, Event::PlacementChanged { .. }));

        snapshot.windows[0].mode = WindowMode::Fullscreen;
        snapshot.windows[0].placement = WindowPlacement::Fullscreen {
            restore: FullscreenRestore::Tiled {
                world_geometry: Rect::new(5, 5, 20, 20),
            },
        };
        let events = hub.publish(snapshot);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].event, Event::WindowChanged { .. }));
    }

    #[test]
    fn global_focus_change_has_a_dedicated_event() {
        let mut hub = EventHub::default();
        hub.publish(DesktopSnapshot::default());
        let snapshot = DesktopSnapshot {
            focused_window: Some(WindowId(9)),
            ..DesktopSnapshot::default()
        };
        let events = hub.publish(snapshot);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event,
            Event::WindowFocusChanged {
                id: Some(WindowId(9))
            }
        );
    }

    #[test]
    fn sequence_overflow_starts_a_new_epoch_for_the_broadcaster() {
        let mut hub = EventHub {
            sequence: u64::MAX,
            ..EventHub::default()
        };
        hub.config_loaded(1, false, None);
        let events = hub.publish(DesktopSnapshot::default());
        assert_eq!(events[0].sequence, 1);
        assert!(hub.take_sequence_overflow());
        assert!(!hub.take_sequence_overflow());
    }
}
