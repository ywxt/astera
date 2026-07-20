use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Direction, Rect};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WindowId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WorkspaceId(pub u32);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Placement {
    #[default]
    Tiled,
    Floating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CameraPolicy {
    Centered,
    KeepVisible { margin: i64 },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub id: WindowId,
    pub geometry: Rect,
    pub placement: Placement,
}

impl Window {
    pub fn tiled(id: WindowId, geometry: Rect) -> Self {
        Self {
            id,
            geometry,
            placement: Placement::Tiled,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub windows: BTreeMap<WindowId, Window>,
    pub focus: Option<WindowId>,
    pub focus_direction: Direction,
    pub generation: u64,
}

impl Workspace {
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            windows: BTreeMap::new(),
            focus: None,
            focus_direction: Direction::RIGHT,
            generation: 0,
        }
    }

    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.get(&id)
    }

    pub fn tiled_windows_are_stable(&self, gap: i64) -> bool {
        let tiled: Vec<_> = self
            .windows
            .values()
            .filter(|window| window.placement == Placement::Tiled)
            .collect();

        tiled.iter().enumerate().all(|(index, window)| {
            tiled[index + 1..]
                .iter()
                .all(|other| !window.geometry.conflicts(other.geometry, gap))
        })
    }
}
