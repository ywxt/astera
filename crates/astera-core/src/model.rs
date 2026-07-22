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
    pub policy: CameraPolicy,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            center: Point::ORIGIN,
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
pub struct ViewportPlacement {
    pub rect: Rect,
    pub normalized_center: NormalizedPoint,
    pub output_rects: BTreeMap<String, Rect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NormalizedPoint {
    pub x_millionths: u32,
    pub y_millionths: u32,
}

impl ViewportPlacement {
    pub fn new(rect: Rect, viewport: Size) -> Self {
        Self {
            rect,
            normalized_center: NormalizedPoint::from_rect(rect, viewport),
            output_rects: BTreeMap::new(),
        }
    }

    pub fn store_for_output(&mut self, output: &str, viewport: Size) {
        self.normalized_center = NormalizedPoint::from_rect(self.rect, viewport);
        self.output_rects.insert(output.to_owned(), self.rect);
    }
}

impl NormalizedPoint {
    const DENOMINATOR: f64 = 1_000_000.0;

    pub fn from_rect(rect: Rect, viewport: Size) -> Self {
        let center = rect.center();
        Self {
            x_millionths: normalize_axis(center.x, viewport.width),
            y_millionths: normalize_axis(center.y, viewport.height),
        }
    }

    pub fn center_in(self, viewport: Size) -> Point {
        Point::new(
            (self.x_millionths as f64 / Self::DENOMINATOR * viewport.width as f64).round() as i64,
            (self.y_millionths as f64 / Self::DENOMINATOR * viewport.height as f64).round() as i64,
        )
    }
}

fn normalize_axis(value: i64, extent: i64) -> u32 {
    if extent <= 0 {
        return 500_000;
    }
    ((value as f64 / extent as f64).clamp(0.0, 1.0) * NormalizedPoint::DENOMINATOR).round() as u32
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FloatingPlacement {
    pub window: WindowId,
    pub viewport: ViewportPlacement,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RestorePlacement {
    Tiled { world_rect: Rect },
    Floating { viewport: ViewportPlacement },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FullscreenPlacement {
    pub window: WindowId,
    pub restore: RestorePlacement,
}

impl FullscreenPlacement {
    pub fn size(&self) -> Size {
        match &self.restore {
            RestorePlacement::Tiled { world_rect } => world_rect.size,
            RestorePlacement::Floating { viewport } => viewport.rect.size,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: Option<String>,
    pub original_output: Option<String>,
    pub tiled: BTreeMap<WindowId, TiledWindow>,
    pub floating: BTreeMap<WindowId, FloatingPlacement>,
    pub fullscreen: Option<FullscreenPlacement>,
    pub camera: CameraState,
    pub focused_window: Option<WindowId>,
    pub focus_history: Vec<WindowId>,
    pub layout_direction_hint: Direction,
    pub generation: u64,
}

impl Workspace {
    pub fn new(id: WorkspaceId) -> Self {
        Self {
            id,
            name: None,
            original_output: None,
            tiled: BTreeMap::new(),
            floating: BTreeMap::new(),
            fullscreen: None,
            camera: CameraState::default(),
            focused_window: None,
            focus_history: Vec::new(),
            layout_direction_hint: Direction::RIGHT,
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
            WindowMode::Floating => Some(self.floating[&id].viewport.rect.size),
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

    /// Moves the camera according to its policy so the focused tiled window is visible.
    /// Floating and fullscreen windows are viewport-local and never move the camera.
    pub fn follow_focus(&mut self, viewport_size: Size) -> bool {
        let Some(focused) = self.focused_window else {
            return false;
        };
        let Some(rect) = self.tiled.get(&focused).map(|window| window.geometry) else {
            return false;
        };
        let old_center = self.camera.center;
        match self.camera.policy {
            CameraPolicy::Centered => self.camera.center = rect.center(),
            CameraPolicy::KeepVisible { margin } => {
                let half_width = viewport_size.width as f64 / 2.0;
                let half_height = viewport_size.height as f64 / 2.0;
                let margin = margin.max(0) as f64;
                let available_width = (half_width * 2.0 - margin * 2.0).max(0.0);
                let available_height = (half_height * 2.0 - margin * 2.0).max(0.0);
                if rect.size.width as f64 > available_width {
                    self.camera.center.x = rect.center().x;
                } else {
                    let left = self.camera.center.x as f64 - half_width + margin;
                    let right = self.camera.center.x as f64 + half_width - margin;
                    if (rect.origin.x as f64) < left {
                        self.camera.center.x =
                            (rect.origin.x as f64 - margin + half_width).round() as i64;
                    } else if (rect.origin.x + rect.size.width) as f64 > right {
                        self.camera.center.x =
                            (rect.origin.x as f64 + rect.size.width as f64 + margin - half_width)
                                .round() as i64;
                    }
                }
                if rect.size.height as f64 > available_height {
                    self.camera.center.y = rect.center().y;
                } else {
                    let top = self.camera.center.y as f64 - half_height + margin;
                    let bottom = self.camera.center.y as f64 + half_height - margin;
                    if (rect.origin.y as f64) < top {
                        self.camera.center.y =
                            (rect.origin.y as f64 - margin + half_height).round() as i64;
                    } else if (rect.origin.y + rect.size.height) as f64 > bottom {
                        self.camera.center.y =
                            (rect.origin.y as f64 + rect.size.height as f64 + margin - half_height)
                                .round() as i64;
                    }
                }
            }
        }
        self.camera.center != old_center
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
            physical_size: logical_size,
            logical_size,
            native_scale: Scale120::ONE,
            transform: OutputTransform::Normal,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputWorkspaceSet {
    pub output: Output,
    pub workspaces: Vec<Workspace>,
    pub active: usize,
}

impl OutputWorkspaceSet {
    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.get(self.active)
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.get_mut(self.active)
    }
}
