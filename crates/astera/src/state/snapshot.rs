use std::{fs, path::PathBuf};

use astera_core::{Rect, WindowMode};

use super::geometry::mode_layer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rgb(u8, u8, u8);

#[derive(Clone, Copy)]
struct SolidElement {
    mode: WindowMode,
    rect: Rect,
    color: Rgb,
}

struct SoftwareFrame {
    width: usize,
    height: usize,
    pixels: Vec<Rgb>,
}

impl SoftwareFrame {
    fn new(width: usize, height: usize, clear: Rgb) -> Self {
        Self {
            width,
            height,
            pixels: vec![clear; width * height],
        }
    }

    fn draw(&mut self, elements: &mut [SolidElement]) {
        elements.sort_by_key(|element| mode_layer(element.mode));
        for element in elements {
            let left = element.rect.origin.x.clamp(0, self.width as i64) as usize;
            let top = element.rect.origin.y.clamp(0, self.height as i64) as usize;
            let right = (element.rect.origin.x + element.rect.size.width)
                .clamp(0, self.width as i64) as usize;
            let bottom = (element.rect.origin.y + element.rect.size.height)
                .clamp(0, self.height as i64) as usize;
            for y in top..bottom {
                for x in left..right {
                    self.pixels[y * self.width + x] = element.color;
                }
            }
        }
    }

    fn ppm(&self) -> String {
        let mut ppm = format!("P3\n{} {}\n255\n", self.width, self.height);
        for row in self.pixels.chunks(self.width) {
            for (index, Rgb(red, green, blue)) in row.iter().enumerate() {
                if index != 0 {
                    ppm.push(' ');
                }
                ppm.push_str(&format!("{red} {green} {blue}"));
            }
            ppm.push('\n');
        }
        ppm
    }
}

fn assert_snapshot(name: &str, actual: &str) {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected_path = manifest.join("tests/golden").join(format!("{name}.ppm"));
    let expected = fs::read_to_string(&expected_path).expect("golden snapshot must exist");
    if expected == actual {
        return;
    }

    let artifact = manifest.join("../../target/render-artifacts").join(name);
    fs::create_dir_all(&artifact).expect("render artifact directory must be writable");
    fs::write(artifact.join("expected.ppm"), &expected).unwrap();
    fs::write(artifact.join("actual.ppm"), actual).unwrap();
    fs::write(
        artifact.join("diff.txt"),
        first_difference(&expected, actual),
    )
    .unwrap();
    panic!(
        "render snapshot {name:?} changed; inspect {}",
        artifact.display()
    );
}

fn first_difference(expected: &str, actual: &str) -> String {
    let expected = expected.lines().collect::<Vec<_>>();
    let actual = actual.lines().collect::<Vec<_>>();
    let line = expected
        .iter()
        .zip(&actual)
        .position(|(expected, actual)| expected != actual)
        .unwrap_or(expected.len().min(actual.len()));
    format!(
        "first differing line: {}\nexpected: {}\nactual:   {}\n",
        line + 1,
        expected.get(line).copied().unwrap_or("<missing>"),
        actual.get(line).copied().unwrap_or("<missing>"),
    )
}

#[test]
fn window_modes_have_deterministic_pixel_order_and_clipping() {
    let mut frame = SoftwareFrame::new(6, 4, Rgb(6, 9, 15));
    // Intentionally reverse the input order: draw() must derive compositor layer order.
    let mut elements = [
        SolidElement {
            mode: WindowMode::Floating,
            rect: Rect::new(2, 1, 6, 4),
            color: Rgb(0, 180, 90),
        },
        SolidElement {
            mode: WindowMode::Tiled,
            rect: Rect::new(0, 0, 4, 3),
            color: Rgb(220, 45, 55),
        },
    ];
    frame.draw(&mut elements);
    assert_snapshot("window_layers", &frame.ppm());
}

#[test]
fn fullscreen_covers_the_complete_viewport() {
    let mut frame = SoftwareFrame::new(6, 4, Rgb(6, 9, 15));
    let mut elements = [
        SolidElement {
            mode: WindowMode::Fullscreen,
            rect: Rect::new(-4, -4, 20, 20),
            color: Rgb(30, 80, 220),
        },
        SolidElement {
            mode: WindowMode::Floating,
            rect: Rect::new(2, 1, 3, 2),
            color: Rgb(0, 180, 90),
        },
    ];
    frame.draw(&mut elements);
    assert_snapshot("fullscreen", &frame.ppm());
}
