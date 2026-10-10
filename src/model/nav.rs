//! The nav's live geometry and attachment position shared across runtime layers.

/// Which way the two views stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewLayout {
    Vertical,
    Horizontal,
}

/// Which side of the terminal view the nav is attached to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavPosition {
    Left,
    Top,
    Right,
    Bottom,
    /// The nav as a floating box over the terminal, kept near the right edge and
    /// auto-placed over the terminal's empty space.
    Floating,
}

/// The nav's live size as one value, never loose values: the effective width has a single
/// owner, and every geometry (the draw, the PTY sizing, the mouse hit-test) is cut from
/// this one value, so a resize while xmux runs cannot reach one consumer and miss another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavSize {
    /// The width the user set.
    pub natural: u16,
    /// The width on screen this frame.
    pub width: u16,
    /// The horizontal nav's height the user set; 0 means auto.
    pub height: u16,
    /// Which side of the terminal view the nav is attached to this frame. Auto-hide
    /// keeps it, so a hidden nav returns on the side it left.
    pub position: NavPosition,
    /// Whether the nav shows only its prefix hint.
    pub collapsed: bool,
    /// The floating nav's on-screen box, set by the app from the terminal's empty space
    /// when `position` is [`NavPosition::Floating`]. `None` when not floating.
    pub floating: Option<ratatui::layout::Rect>,
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
            floating: None,
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
            floating: None,
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

    /// The same nav with its floating box set.
    pub fn with_floating(self, floating: Option<ratatui::layout::Rect>) -> Self {
        Self { floating, ..self }
    }
}

/// One step of the attachment-position cycle.
pub fn step_nav_position(
    pinned: Option<NavPosition>,
    effective: NavPosition,
) -> Option<NavPosition> {
    match pinned {
        Some(position) => Some(position.clockwise()),
        None => Some(effective.clockwise()),
    }
}

impl NavPosition {
    /// The view stacking this placement produces.
    pub fn layout(self) -> ViewLayout {
        match self {
            Self::Left | Self::Right | Self::Floating => ViewLayout::Vertical,
            Self::Top | Self::Bottom => ViewLayout::Horizontal,
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
            Self::Bottom => Self::Floating,
            Self::Floating => Self::Left,
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
            "floating" => Some(Self::Floating),
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
            Self::Floating => "floating",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_maps_vertical_and_horizontal_navs() {
        assert_eq!(NavPosition::Left.layout(), ViewLayout::Vertical);
        assert_eq!(NavPosition::Right.layout(), ViewLayout::Vertical);
        assert_eq!(NavPosition::Floating.layout(), ViewLayout::Vertical);
        assert_eq!(NavPosition::Top.layout(), ViewLayout::Horizontal);
        assert_eq!(NavPosition::Bottom.layout(), ViewLayout::Horizontal);
    }

    #[test]
    fn clockwise_steps_one_side_round() {
        assert_eq!(NavPosition::Left.clockwise(), NavPosition::Top);
        assert_eq!(NavPosition::Top.clockwise(), NavPosition::Right);
        assert_eq!(NavPosition::Right.clockwise(), NavPosition::Bottom);
        assert_eq!(NavPosition::Bottom.clockwise(), NavPosition::Floating);
        assert_eq!(NavPosition::Floating.clockwise(), NavPosition::Left);
    }

    #[test]
    fn forward_arrows_face_the_terminal_on_left_and_top() {
        assert!(NavPosition::Left.forward_arrows_face_terminal());
        assert!(NavPosition::Top.forward_arrows_face_terminal());
        assert!(!NavPosition::Right.forward_arrows_face_terminal());
        assert!(!NavPosition::Bottom.forward_arrows_face_terminal());
        assert!(!NavPosition::Floating.forward_arrows_face_terminal());
    }

    #[test]
    fn parse_reads_the_five_words() {
        assert_eq!(NavPosition::parse("left"), Some(NavPosition::Left));
        assert_eq!(NavPosition::parse(" top "), Some(NavPosition::Top));
        assert_eq!(NavPosition::parse("Right"), Some(NavPosition::Right));
        assert_eq!(NavPosition::parse("\nbottom\n"), Some(NavPosition::Bottom));
        assert_eq!(NavPosition::parse("Floating"), Some(NavPosition::Floating));
        assert_eq!(NavPosition::parse("diagonal"), None);
        assert_eq!(NavPosition::parse(""), None);
    }

    #[test]
    fn step_nav_position_cycles_one_step_clockwise_without_unpin() {
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
            Some(NavPosition::Floating)
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
            Some(NavPosition::Floating),
            "the fifth step goes floating, never unpinning"
        );
        assert_eq!(
            step_nav_position(Some(NavPosition::Floating), NavPosition::Right),
            Some(NavPosition::Left),
            "the floating step wraps back to left"
        );
    }

    #[test]
    fn word_round_trips_through_parse() {
        for position in [
            NavPosition::Left,
            NavPosition::Top,
            NavPosition::Right,
            NavPosition::Bottom,
            NavPosition::Floating,
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
