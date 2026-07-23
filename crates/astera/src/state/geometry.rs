use astera_core::{OutputTransform, Point, Size, WindowMode};
use smithay::{
    utils::{Logical, Physical, Point as SmithayPoint},
    wayland::shell::wlr_layer::Layer,
};

pub(super) fn mode_layer(mode: WindowMode) -> u8 {
    // Values intentionally leave slots for layer-shell surfaces between window classes.
    match mode {
        WindowMode::Tiled => 2,
        WindowMode::Floating | WindowMode::Maximized => 3,
        WindowMode::Fullscreen => 5,
    }
}

pub(super) fn layer_rank(layer: Layer) -> u8 {
    match layer {
        Layer::Background => 0,
        Layer::Bottom => 1,
        Layer::Top => 4,
        Layer::Overlay => 6,
    }
}

pub(super) fn output_transform(transform: OutputTransform) -> smithay::utils::Transform {
    match transform {
        OutputTransform::Normal => smithay::utils::Transform::Normal,
        OutputTransform::Rotate90 => smithay::utils::Transform::_90,
        OutputTransform::Rotate180 => smithay::utils::Transform::_180,
        OutputTransform::Rotate270 => smithay::utils::Transform::_270,
        OutputTransform::Flipped => smithay::utils::Transform::Flipped,
    }
}

pub(super) fn point_inside(
    point: SmithayPoint<f64, Logical>,
    origin: Point,
    size: Size,
    scale: f64,
) -> bool {
    point.x >= origin.x as f64
        && point.x < origin.x as f64 + size.width as f64 * scale
        && point.y >= origin.y as f64
        && point.y < origin.y as f64 + size.height as f64 * scale
}

pub(super) fn saturating_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

pub(super) fn physical_point(origin: Point, scale: f64) -> SmithayPoint<i32, Physical> {
    // Renderer coordinates are bounded to i32 even though the infinite world uses i64.
    (
        (origin.x as f64 * scale)
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32,
        (origin.y as f64 * scale)
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32,
    )
        .into()
}
