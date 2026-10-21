use crate::Position;

/// pixels the cursor is kept away from the edge it returns through, so it
/// neither re-triggers the barrier nor lands on an auto-hide taskbar trigger
pub(crate) const RETURN_INSET: i32 = 2;

/// a display's pixel rectangle; `right` and `bottom` are exclusive
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Bounds {
    fn contains(&self, (x, y): (i32, i32)) -> bool {
        self.left <= x && x < self.right && self.top <= y && y < self.bottom
    }

    fn clamp(&self, (x, y): (i32, i32)) -> (i32, i32) {
        (
            x.clamp(self.left, self.right - 1),
            y.clamp(self.top, self.bottom - 1),
        )
    }
}

fn display_containing(displays: &[Bounds], point: (i32, i32)) -> Option<&Bounds> {
    displays
        .iter()
        .find(|display| display.contains(point))
        .or_else(|| displays.first())
}

/// center of the display the cursor was on when it left; the cursor is
/// parked there while another computer is controlled
pub(crate) fn parking_point(displays: &[Bounds], from: (i32, i32)) -> Option<(i32, i32)> {
    let display = display_containing(displays, from)?;
    Some((
        display.left + (display.right - display.left) / 2,
        display.top + (display.bottom - display.top) / 2,
    ))
}

/// where the cursor reappears when control returns: the point it left from,
/// moved `RETURN_INSET` pixels away from the edge it crossed
pub(crate) fn return_point(
    displays: &[Bounds],
    left_from: (i32, i32),
    edge: Position,
) -> (i32, i32) {
    let (x, y) = left_from;
    let inset = match edge {
        Position::Left => (x + RETURN_INSET, y),
        Position::Right => (x - RETURN_INSET, y),
        Position::Top => (x, y + RETURN_INSET),
        Position::Bottom => (x, y - RETURN_INSET),
    };
    match display_containing(displays, left_from) {
        Some(display) => display.clamp(inset),
        None => inset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Bounds = Bounds {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    const SIDE: Bounds = Bounds {
        left: 1920,
        top: -200,
        right: 3840,
        bottom: 880,
    };

    #[test]
    fn parking_point_is_the_center_of_the_display_it_left_from() {
        assert_eq!(parking_point(&[MAIN, SIDE], (1919, 500)), Some((960, 540)));
        assert_eq!(parking_point(&[MAIN, SIDE], (3839, 100)), Some((2880, 340)));
    }

    #[test]
    fn parking_point_falls_back_to_the_first_display() {
        assert_eq!(parking_point(&[MAIN, SIDE], (-50, -50)), Some((960, 540)));
        assert_eq!(parking_point(&[], (0, 0)), None);
    }

    #[test]
    fn return_point_moves_away_from_the_crossed_edge() {
        let displays = [MAIN];
        assert_eq!(
            return_point(&displays, (1919, 400), Position::Right),
            (1917, 400)
        );
        assert_eq!(return_point(&displays, (0, 400), Position::Left), (2, 400));
        assert_eq!(return_point(&displays, (700, 0), Position::Top), (700, 2));
        assert_eq!(
            return_point(&displays, (700, 1079), Position::Bottom),
            (700, 1077)
        );
    }

    #[test]
    fn return_point_keeps_the_height_it_left_at() {
        assert_eq!(return_point(&[MAIN], (1919, 733), Position::Right).1, 733);
    }

    #[test]
    fn return_point_stays_inside_a_small_display() {
        let tiny = Bounds {
            left: 0,
            top: 0,
            right: 2,
            bottom: 2,
        };
        assert_eq!(return_point(&[tiny], (1, 1), Position::Right), (0, 1));
        assert_eq!(return_point(&[tiny], (0, 0), Position::Left), (1, 0));
    }

    #[test]
    fn return_point_uses_the_display_it_left_from() {
        let displays = [MAIN, SIDE];
        assert_eq!(
            return_point(&displays, (3839, 0), Position::Right),
            (3837, 0)
        );
    }
}
