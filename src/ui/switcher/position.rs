//! The nav's attachment position: which side of the terminal view the nav rides on,
//! and the `prefix p` cycle that pins it.

use super::ViewLayout;

/// Which side of the terminal view the nav is attached to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavPosition {
    Left,
    Top,
    Right,
    Bottom,
}

/// One step of the `prefix p` cycle: left → top → right → bottom → unpin. Unpinned,
/// the first step goes to the clockwise neighbour of the CURRENT effective position, so
/// no press is ever an invisible no-op; pinned, the pin's own neighbour decides.
pub fn step_nav_position(
    pinned: Option<NavPosition>,
    effective: NavPosition,
) -> Option<NavPosition> {
    match pinned {
        Some(NavPosition::Bottom) => None,
        Some(p) => Some(p.clockwise()),
        None => Some(effective.clockwise()),
    }
}

impl NavPosition {
    /// The view stacking this placement produces: the two columns (left or right) or
    /// the two bands (top or bottom).
    pub fn layout(self) -> ViewLayout {
        match self {
            NavPosition::Left | NavPosition::Right => ViewLayout::Column,
            NavPosition::Top | NavPosition::Bottom => ViewLayout::Band,
        }
    }

    /// Whether the arrow pair facing the terminal's side is the forward pair (right and
    /// down). With the nav on the left or above, forward names the terminal; with the
    /// nav on the right or below, the pair flips and backward names it.
    pub fn forward_arrows_face_terminal(self) -> bool {
        matches!(self, NavPosition::Left | NavPosition::Top)
    }

    /// The one step clockwise: left, top, right, bottom, back to left.
    pub fn clockwise(self) -> NavPosition {
        match self {
            NavPosition::Left => NavPosition::Top,
            NavPosition::Top => NavPosition::Right,
            NavPosition::Right => NavPosition::Bottom,
            NavPosition::Bottom => NavPosition::Left,
        }
    }

    /// Parses a persisted or configured word: `left`, `top`, `right`, or `bottom`.
    #[allow(clippy::should_implement_trait)] // intentionally not FromStr: returns Option, not Result
    pub fn parse(s: &str) -> Option<NavPosition> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" => Some(NavPosition::Left),
            "top" => Some(NavPosition::Top),
            "right" => Some(NavPosition::Right),
            "bottom" => Some(NavPosition::Bottom),
            _ => None,
        }
    }

    /// The word this position writes to the pref file.
    pub fn word(self) -> &'static str {
        match self {
            NavPosition::Left => "left",
            NavPosition::Top => "top",
            NavPosition::Right => "right",
            NavPosition::Bottom => "bottom",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::NavSize;
    use super::*;

    #[test]
    fn layout_maps_columns_and_bands() {
        assert_eq!(NavPosition::Left.layout(), ViewLayout::Column);
        assert_eq!(NavPosition::Right.layout(), ViewLayout::Column);
        assert_eq!(NavPosition::Top.layout(), ViewLayout::Band);
        assert_eq!(NavPosition::Bottom.layout(), ViewLayout::Band);
    }

    #[test]
    fn clockwise_steps_one_side_round() {
        assert_eq!(NavPosition::Left.clockwise(), NavPosition::Top);
        assert_eq!(NavPosition::Top.clockwise(), NavPosition::Right);
        assert_eq!(NavPosition::Right.clockwise(), NavPosition::Bottom);
        assert_eq!(NavPosition::Bottom.clockwise(), NavPosition::Left);
    }

    #[test]
    fn forward_arrows_face_the_terminal_on_left_and_top() {
        assert!(NavPosition::Left.forward_arrows_face_terminal());
        assert!(NavPosition::Top.forward_arrows_face_terminal());
        assert!(!NavPosition::Right.forward_arrows_face_terminal());
        assert!(!NavPosition::Bottom.forward_arrows_face_terminal());
    }

    #[test]
    fn parse_reads_the_four_words() {
        assert_eq!(NavPosition::parse("left"), Some(NavPosition::Left));
        assert_eq!(NavPosition::parse(" top "), Some(NavPosition::Top));
        assert_eq!(NavPosition::parse("Right"), Some(NavPosition::Right));
        assert_eq!(NavPosition::parse("\nbottom\n"), Some(NavPosition::Bottom));
        assert_eq!(NavPosition::parse("diagonal"), None);
        assert_eq!(NavPosition::parse(""), None);
    }

    #[test]
    fn step_nav_position_cycles_one_step_clockwise() {
        // Unpinned: the first step goes to the clockwise neighbour of the CURRENT
        // effective position, so no press is ever an invisible no-op.
        assert_eq!(
            step_nav_position(None, NavPosition::Left),
            Some(NavPosition::Top)
        );
        assert_eq!(
            step_nav_position(None, NavPosition::Top),
            Some(NavPosition::Right)
        );
        assert_eq!(
            step_nav_position(None, NavPosition::Right),
            Some(NavPosition::Bottom)
        );
        assert_eq!(
            step_nav_position(None, NavPosition::Bottom),
            Some(NavPosition::Left)
        );
        // Pinned: the pin's own clockwise neighbour, whatever is on screen now; the
        // bottom pin's step unpins, leaving the keyboard path back to auto.
        assert_eq!(
            step_nav_position(Some(NavPosition::Left), NavPosition::Bottom),
            Some(NavPosition::Top)
        );
        assert_eq!(
            step_nav_position(Some(NavPosition::Top), NavPosition::Right),
            Some(NavPosition::Right)
        );
        assert_eq!(
            step_nav_position(Some(NavPosition::Right), NavPosition::Left),
            Some(NavPosition::Bottom)
        );
        assert_eq!(
            step_nav_position(Some(NavPosition::Bottom), NavPosition::Right),
            None,
            "the fifth step unpins"
        );
    }

    #[test]
    fn word_round_trips_through_parse() {
        for p in [
            NavPosition::Left,
            NavPosition::Top,
            NavPosition::Right,
            NavPosition::Bottom,
        ] {
            assert_eq!(NavPosition::parse(p.word()), Some(p));
        }
    }

    #[test]
    fn the_position_rides_in_nav_size_as_the_fourth_component() {
        assert_eq!(
            NavSize::visible(48).position,
            NavPosition::Left,
            "a fresh NavSize defaults to Left"
        );
        let nav = NavSize::visible(48).with_position(NavPosition::Right);
        assert_eq!(nav.position, NavPosition::Right);
        assert_eq!(nav.natural, 48);
        assert_eq!(nav.width, 48);
        let hidden = NavSize::hidden(48).with_position(NavPosition::Bottom);
        assert_eq!(hidden.position, NavPosition::Bottom);
        assert_eq!(hidden.width, 0);
    }
}
