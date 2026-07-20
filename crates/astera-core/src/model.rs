use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Direction, Point, Rect, Size};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WindowId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WorkspaceId(pub u32);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct OutputId(pub u32);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum WindowMode {
    #[default]
    Tiled,
    Floating,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CameraPolicy {
    Centered,
    KeepVisible { margin: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraState {
    pub center: Point,
    pub zoom: f64,
    pub policy: CameraPolicy,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            center: Point::ORIGIN,
            zoom: 1.0,
            policy: CameraPolicy::KeepVisible { margin: 32 },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TiledWindow {
    pub id: WindowId,
    pub geometry: Rect,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FloatingPlacement {
    pub window: WindowId,
    pub rect: Rect,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RestorePlacement {
    Tiled { world_rect: Rect },
    Floating { viewport_rect: Rect },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FullscreenPlacement {
    pub window: WindowId,
    pub restore: RestorePlacement,
}

impl FullscreenPlacement {
    pub fn size(&self) -> Size {
        match self.restore {
            RestorePlacement::Tiled { world_rect } => world_rect.size,
            RestorePlacement::Floating { viewport_rect } => viewport_rect.size,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub bound_output: Option<OutputId>,
    pub tiled: BTreeMap<WindowId, TiledWindow>,
    pub floating: BTreeMap<WindowId, FloatingPlacement>,
    pub fullscreen: Option<FullscreenPlacement>,
    pub camera: CameraState,
    pub focused_window: Option<WindowId>,
    pub focus_history: Vec<WindowId>,
    pub focus_direction: Direction,
    pub generation: u64,
}

impl Workspace {
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            bound_output: None,
            tiled: BTreeMap::new(),
            floating: BTreeMap::new(),
            fullscreen: None,
            camera: CameraState::default(),
            focused_window: None,
            focus_history: Vec::new(),
            focus_direction: Direction::RIGHT,
            generation: 0,
        }
    }

    pub fn tiled_window(&self, id: WindowId) -> Option<&TiledWindow> {
        self.tiled.get(&id)
    }

    pub fn window_mode(&self, id: WindowId) -> Option<WindowMode> {
        if self.tiled.contains_key(&id) {
            Some(WindowMode::Tiled)
        } else if self.floating.contains_key(&id) {
            Some(WindowMode::Floating)
        } else if self
            .fullscreen
            .as_ref()
            .is_some_and(|full| full.window == id)
        {
            Some(WindowMode::Fullscreen)
        } else {
            None
        }
    }

    pub fn contains_window(&self, id: WindowId) -> bool {
        self.window_mode(id).is_some()
    }

    pub fn window_size(&self, id: WindowId) -> Option<Size> {
        match self.window_mode(id)? {
            WindowMode::Tiled => Some(self.tiled[&id].geometry.size),
            WindowMode::Floating => Some(self.floating[&id].rect.size),
            WindowMode::Fullscreen => self.fullscreen.as_ref().map(FullscreenPlacement::size),
        }
    }

    pub fn focus(&mut self, id: WindowId) -> bool {
        if !self.contains_window(id) {
            return false;
        }
        self.focus_history.retain(|candidate| *candidate != id);
        self.focus_history.push(id);
        self.focused_window = Some(id);
        true
    }

    pub fn remove_focus(&mut self, id: WindowId) {
        self.focus_history.retain(|candidate| *candidate != id);
        if self.focused_window == Some(id) {
            self.focused_window = self
                .focus_history
                .iter()
                .rev()
                .copied()
                .find(|candidate| self.contains_window(*candidate));
        }
    }

    pub fn tiled_windows_are_stable(&self, gap: i64) -> bool {
        let tiled: Vec<_> = self.tiled.values().collect();
        tiled.iter().enumerate().all(|(index, window)| {
            tiled[index + 1..]
                .iter()
                .all(|other| !window.geometry.conflicts(other.geometry, gap))
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum OutputTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flipped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Scale120(pub u32);

impl Scale120 {
    pub const ONE: Self = Self(120);
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub id: OutputId,
    pub stable_key: String,
    pub current_workspace: Option<WorkspaceId>,
    pub physical_size: Size,
    pub logical_size: Size,
    pub native_scale: Scale120,
    pub transform: OutputTransform,
}

impl Output {
    pub fn new(id: OutputId, stable_key: impl Into<String>, logical_size: Size) -> Self {
        Self {
            id,
            stable_key: stable_key.into(),
            current_workspace: None,
            physical_size: logical_size,
            logical_size,
            native_scale: Scale120::ONE,
            transform: OutputTransform::Normal,
        }
    }
}
