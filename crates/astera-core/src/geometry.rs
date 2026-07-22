use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: i64,
    pub y: i64,
}

impl Point {
    pub const ORIGIN: Self = Self { x: 0, y: 0 };

    pub const fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: i64,
    pub height: i64,
}

impl Size {
    pub const fn new(width: i64, height: i64) -> Self {
        Self { width, height }
    }

    pub const fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub const fn new(x: i64, y: i64, width: i64, height: i64) -> Self {
        Self {
            origin: Point::new(x, y),
            size: Size::new(width, height),
        }
    }

    pub fn centered_at(center: Point, size: Size) -> Self {
        Self::new(
            center.x - size.width / 2,
            center.y - size.height / 2,
            size.width,
            size.height,
        )
    }

    pub fn center(self) -> Point {
        Point::new(
            self.origin.x + self.size.width / 2,
            self.origin.y + self.size.height / 2,
        )
    }

    pub fn translated(self, x: i64, y: i64) -> Self {
        Self::new(x, y, self.size.width, self.size.height)
    }

    pub fn conflicts(self, other: Self, gap: i64) -> bool {
        self.origin.x < other.origin.x + other.size.width + gap
            && self.origin.x + self.size.width + gap > other.origin.x
            && self.origin.y < other.origin.y + other.size.height + gap
            && self.origin.y + self.size.height + gap > other.origin.y
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Direction {
    pub x: f64,
    pub y: f64,
}

impl Direction {
    pub const RIGHT: Self = Self { x: 1.0, y: 0.0 };

    pub fn new(x: f64, y: f64) -> Self {
        let length = x.hypot(y);
        if !length.is_finite() || length <= f64::EPSILON {
            Self::RIGHT
        } else {
            Self {
                x: x / length,
                y: y / length,
            }
        }
    }

    pub fn between(from: Point, to: Point, fallback: Self) -> Self {
        if from == to {
            fallback.normalized()
        } else {
            Self::new((to.x - from.x) as f64, (to.y - from.y) as f64)
        }
    }

    pub fn normalized(self) -> Self {
        Self::new(self.x, self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_respects_gap_and_touching_boundary() {
        let left = Rect::new(0, 0, 100, 100);
        assert!(!left.conflicts(Rect::new(100, 0, 50, 50), 0));
        assert!(left.conflicts(Rect::new(100, 0, 50, 50), 1));
        assert!(!left.conflicts(Rect::new(0, 101, 50, 50), 1));
    }

    #[test]
    fn direction_normalizes_and_falls_back_for_invalid_vectors() {
        let direction = Direction::new(3.0, 4.0);
        assert!((direction.x - 0.6).abs() < f64::EPSILON);
        assert!((direction.y - 0.8).abs() < f64::EPSILON);
        assert_eq!(Direction::new(0.0, 0.0), Direction::RIGHT);
        assert_eq!(Direction::new(f64::NAN, 1.0), Direction::RIGHT);
        assert_eq!(
            Direction::between(Point::ORIGIN, Point::ORIGIN, direction),
            direction
        );
        assert_eq!(
            Direction::between(Point::ORIGIN, Point::new(0, -5), Direction::RIGHT),
            Direction::new(0.0, -1.0)
        );
    }

    #[test]
    fn centered_and_translated_rect_preserve_size() {
        let rect = Rect::centered_at(Point::new(50, 40), Size::new(21, 11));
        assert_eq!(rect, Rect::new(40, 35, 21, 11));
        assert_eq!(rect.center(), Point::new(50, 40));
        assert_eq!(rect.translated(-10, 12), Rect::new(-10, 12, 21, 11));
        assert!(Size::new(1, 1).is_valid());
        assert!(!Size::new(0, 1).is_valid());
    }
}
