use std::{env, fs};

use astera_core::OutputId;
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point, Size, Transform},
    wayland::compositor,
};

use super::Astera;

#[derive(Clone, Debug)]
pub(super) struct NamedCursor {
    pub(super) buffer: MemoryRenderBuffer,
    pub(super) hotspot: Point<i32, Physical>,
    pub(super) logical_size: Size<i32, Logical>,
    pub(super) source_size: Size<i32, Logical>,
}

#[derive(Clone, Debug)]
pub(crate) enum CursorRenderSource {
    Surface {
        surface: WlSurface,
        location: Point<i32, Physical>,
        scale: f64,
    },
    Memory {
        buffer: MemoryRenderBuffer,
        location: Point<f64, Physical>,
        size: Size<i32, Logical>,
        source_size: Size<i32, Logical>,
    },
}

pub(super) fn load_named_cursor(icon: CursorIcon, scale120: u32) -> NamedCursor {
    load_theme_cursor(icon, scale120).unwrap_or_else(|| fallback_cursor(scale120))
}

fn load_theme_cursor(icon: CursorIcon, scale120: u32) -> Option<NamedCursor> {
    let theme = xcursor::CursorTheme::load(
        &env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".to_owned()),
    );
    let path = [icon.name(), "default", "left_ptr"]
        .into_iter()
        .find_map(|name| theme.load_icon(name))?;
    let bytes = fs::read(path).ok()?;
    let base_size = env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|size| size.parse::<u32>().ok())
        .unwrap_or(24);
    let requested = (base_size * scale120).div_ceil(120);
    let image = xcursor::parser::parse_xcursor(&bytes)?
        .into_iter()
        .min_by_key(|image| image.size.abs_diff(requested))?;
    // DRM ARGB8888 is B,G,R,A in memory on little-endian hosts; XCursor exposes RGBA.
    let mut bgra = Vec::with_capacity(image.pixels_rgba.len());
    for pixel in image.pixels_rgba.as_chunks::<4>().0 {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    Some(NamedCursor {
        buffer: MemoryRenderBuffer::from_slice(
            &bgra,
            Fourcc::Argb8888,
            (image.width as i32, image.height as i32),
            1,
            Transform::Normal,
            None,
        ),
        // The renderer receives a logical destination size while the hotspot remains in the
        // physical pixels of this scale-specific XCursor image.
        hotspot: (image.xhot as i32, image.yhot as i32).into(),
        logical_size: (
            ((image.width * 120) / scale120.max(1)).max(1) as i32,
            ((image.height * 120) / scale120.max(1)).max(1) as i32,
        )
            .into(),
        source_size: (image.width as i32, image.height as i32).into(),
    })
}

fn fallback_cursor(scale120: u32) -> NamedCursor {
    let size = (24 * scale120).div_ceil(120).max(1) as usize;
    let mut bgra = vec![0; size * size * 4];
    for y in 0..(size * 3 / 4) {
        for x in 0..=y / 2 {
            let pixel = (y * size + x) * 4;
            let edge = x == 0 || x == y / 2 || y + 1 == size * 3 / 4;
            let value = if edge { 0 } else { 255 };
            bgra[pixel..pixel + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
    NamedCursor {
        buffer: MemoryRenderBuffer::from_slice(
            &bgra,
            Fourcc::Argb8888,
            (size as i32, size as i32),
            1,
            Transform::Normal,
            None,
        ),
        hotspot: (0, 0).into(),
        logical_size: (24, 24).into(),
        source_size: (size as i32, size as i32).into(),
    }
}

impl Astera {
    pub(super) fn is_cursor_surface(&self, surface: &WlSurface) -> bool {
        matches!(&self.cursor_image_status, CursorImageStatus::Surface(current) if current == surface)
            || self.dnd_icon.as_ref() == Some(surface)
            || self.tablet_tools.values().any(|runtime| {
                matches!(&runtime.cursor_image, CursorImageStatus::Surface(current) if current == surface)
            })
    }

    pub(crate) fn dnd_icon_render_source(
        &self,
        output: OutputId,
    ) -> Option<(WlSurface, Point<i32, Physical>, f64)> {
        let logical_location = if let Some((touch_output, _, location)) = self.dnd_touch_icon {
            if output != touch_output {
                return None;
            }
            location
        } else {
            if output != self.active_output {
                return None;
            }
            self.pointer_location
        };
        if self.session_is_locked() {
            return None;
        }
        let surface = self.dnd_icon.as_ref()?.clone();
        let scale = self.output_scale(output);
        Some((
            surface,
            (
                (logical_location.x * scale).round() as i32,
                (logical_location.y * scale).round() as i32,
            )
                .into(),
            scale,
        ))
    }

    pub(super) fn cursor_surface_for_output(&self, output: OutputId) -> Option<WlSurface> {
        match self.active_cursor_status(output)? {
            CursorImageStatus::Surface(surface) => Some(surface.clone()),
            CursorImageStatus::Hidden | CursorImageStatus::Named(_) => None,
        }
    }

    fn active_cursor_status(&self, output: OutputId) -> Option<&CursorImageStatus> {
        let (status, cursor_output) = if let Some(descriptor) = &self.active_tablet_cursor
            && let Some(runtime) = self.tablet_tools.get(descriptor)
            && let Some((output, _)) = runtime.cursor_location
        {
            (&runtime.cursor_image, output)
        } else {
            (&self.cursor_image_status, self.active_output)
        };
        (output == cursor_output).then_some(status)
    }

    pub(crate) fn cursor_render_source(&mut self, output: OutputId) -> Option<CursorRenderSource> {
        let (status, location, cursor_output) = if let Some(descriptor) = &self.active_tablet_cursor
            && let Some(runtime) = self.tablet_tools.get(descriptor)
            && let Some((output, location)) = runtime.cursor_location
        {
            (runtime.cursor_image.clone(), location, output)
        } else {
            (
                self.cursor_image_status.clone(),
                self.pointer_location,
                self.active_output,
            )
        };
        if output != cursor_output {
            return None;
        }
        let scale120 = self.desktop.outputs.get(&output)?.output.native_scale.0;
        let scale = f64::from(scale120) / 120.0;
        match status {
            CursorImageStatus::Hidden => None,
            CursorImageStatus::Named(icon) => {
                let cursor = self
                    .named_cursors
                    .entry((icon, scale120))
                    .or_insert_with(|| load_named_cursor(icon, scale120));
                Some(CursorRenderSource::Memory {
                    buffer: cursor.buffer.clone(),
                    location: (
                        location.x * scale - f64::from(cursor.hotspot.x),
                        location.y * scale - f64::from(cursor.hotspot.y),
                    )
                        .into(),
                    size: cursor.logical_size,
                    source_size: cursor.source_size,
                })
            }
            CursorImageStatus::Surface(surface) => {
                let hotspot = compositor::with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .and_then(|data| data.lock().ok().map(|attributes| attributes.hotspot))
                        .unwrap_or_default()
                });
                Some(CursorRenderSource::Surface {
                    surface,
                    location: (
                        ((location.x - f64::from(hotspot.x)) * scale).round() as i32,
                        ((location.y - f64::from(hotspot.y)) * scale).round() as i32,
                    )
                        .into(),
                    scale,
                })
            }
        }
    }

    pub(super) fn update_named_cursor(&mut self, image: &CursorImageStatus) {
        let CursorImageStatus::Named(icon) = image else {
            return;
        };
        let scales = self
            .desktop
            .outputs
            .values()
            .map(|state| state.output.native_scale.0)
            .collect::<Vec<_>>();
        for scale120 in scales {
            self.named_cursors
                .entry((*icon, scale120))
                .or_insert_with(|| load_named_cursor(*icon, scale120));
        }
    }
}
