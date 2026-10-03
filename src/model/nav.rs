//! The nav's live geometry and attachment position shared across runtime layers.

/// Which way the two views stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewLayout {
    Column,
    Band,
}

/// Which side of the terminal view the nav is attached to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavPosition {
    Left,
    Top,
    Right,
    Bottom,
}

/// The nav's live size as one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavSize {
    /// The width the user set.
    pub natural: u16,
    /// The width on screen this frame.
    pub width: u16,
    /// The band's height the user set; 0 means auto.
    pub height: u16,
    /// Which side of the terminal view the nav is attached to this frame.
    pub position: NavPosition,
    /// Whether the nav shows only its resting hint bar and collapse button.
    pub collapsed: bool,
}

impl NavSize {
    /// The nav on screen at the width the user set.
    pub fn visible(natural: u16) -> Self {
        Self {
            natural,
            width: natural,
            height: 0,
            position: NavPosition::Left,
            collapsed: false,
        }
    }

    /// The nav hidden while retaining the width the user set.
    pub fn hidden(natural: u16) -> Self {
        Self {
            natural,
            width: 0,
            height: 0,
            position: NavPosition::Left,
            collapsed: false,
        }
    }

    /// The same nav with the band height the user set.
    pub fn with_height(self, height: u16) -> Self {
        Self { height, ..self }
    }

    /// The same nav attached on another side.
    pub fn with_position(self, position: NavPosition) -> Self {
        Self { position, ..self }
    }
}

/// One step of the attachment-position cycle.
pub fn step_nav_position(
    pinned: Option<NavPosition>,
    effective: NavPosition,
) -> Option<NavPosition> {
    match pinned {
        Some(NavPosition::Bottom) => None,
        Some(position) => Some(position.clockwise()),
        None => Some(effective.clockwise()),
    }
}

impl NavPosition {
    /// The view stacking this placement produces.
    pub fn layout(self) -> ViewLayout {
        match self {
            Self::Left | Self::Right => ViewLayout::Column,
            Self::Top | Self::Bottom => ViewLayout::Band,
        }
    }

    /// Whether the arrow pair facing the terminal is right and down.
    pub fn forward_arrows_face_terminal(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }

    /// The next position clockwise.
    pub fn clockwise(self) -> Self {
        match self {
            Self::Left => Self::Top,
            Self::Top => Self::Right,
            Self::Right => Self::Bottom,
            Self::Bottom => Self::Left,
        }
    }

    /// Parses a persisted or configured position.
    #[allow(clippy::should_implement_trait)]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "left" => Some(Self::Left),
            "top" => Some(Self::Top),
            "right" => Some(Self::Right),
            "bottom" => Some(Self::Bottom),
            _ => None,
        }
    }

    /// The word written to preferences and configuration.
    pub fn word(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
        }
    }
}

#[cfg(test)]
mod tests {
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
        for position in [
            NavPosition::Left,
            NavPosition::Top,
            NavPosition::Right,
            NavPosition::Bottom,
        ] {
            assert_eq!(NavPosition::parse(position.word()), Some(position));
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
