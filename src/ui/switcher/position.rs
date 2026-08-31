//! The nav's attachment position: which side of the terminal view the nav rides on,
//! and the rule that picks it each frame from the [ui] settings and a pinned override.

use super::view_layout;
use super::ViewLayout;
use ratatui::layout::Rect;

/// Which side of the terminal view the nav is attached to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavPosition {
    Left,
    Top,
    Right,
    Bottom,
}

/// The [ui] nav-position settings: the defaults the per-frame resolution falls back to
/// when nothing is pinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavPositionSetting {
    /// Whether the wide/narrow turnover picks between `wide` and `narrow`.
    pub auto: bool,
    /// The position when the terminal view is the wider (the column layout).
    pub wide: NavPosition,
    /// The position when the terminal view is not (the band layout).
    pub narrow: NavPosition,
    /// The forced position while `auto` is off; none means the wide default.
    pub force: Option<NavPosition>,
}

impl Default for NavPositionSetting {
    fn default() -> Self {
        NavPositionSetting {
            auto: true,
            wide: NavPosition::Left,
            narrow: NavPosition::Top,
            force: None,
        }
    }
}

/// The position in effect this frame: a pinned override wins outright; otherwise the
/// wide/narrow turnover picks between the two auto settings; with the turnover off, the
/// forced position (or the wide default when none is forced). The wide/narrow judgment
/// reads the area and the nav's natural width only, never the resolved position, so the
/// resolution cannot feed back into its own input and oscillate at the boundary.
pub fn resolve_nav_position(
    setting: &NavPositionSetting,
    pinned: Option<NavPosition>,
    area: Rect,
    nav_natural: u16,
) -> NavPosition {
    if let Some(p) = pinned {
        return p;
    }
    if setting.auto {
        return if view_layout(area, nav_natural) == ViewLayout::Column {
            setting.wide
        } else {
            setting.narrow
        };
    }
    setting.force.unwrap_or(setting.wide)
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
    fn a_pinned_position_wins_unconditionally() {
        let setting = NavPositionSetting::default();
        let landscape = Rect::new(0, 0, 200, 30);
        let portrait = Rect::new(0, 0, 40, 100);
        for p in [
            NavPosition::Left,
            NavPosition::Top,
            NavPosition::Right,
            NavPosition::Bottom,
        ] {
            assert_eq!(resolve_nav_position(&setting, Some(p), landscape, 48), p);
            assert_eq!(resolve_nav_position(&setting, Some(p), portrait, 48), p);
        }
    }

    #[test]
    fn auto_resolves_by_the_wide_narrow_turnover() {
        let setting = NavPositionSetting::default();
        // 140x30 with a 48-wide nav: the column the nav leaves is 91 wide over 30 rows,
        // wider than tall, so the wide (column) default applies.
        assert_eq!(
            resolve_nav_position(&setting, None, Rect::new(0, 0, 140, 30), 48),
            NavPosition::Left
        );
        // 40x100: the nav column would leave the terminal 40 over 100 rows, so the band
        // takes over and the narrow default applies.
        assert_eq!(
            resolve_nav_position(&setting, None, Rect::new(0, 0, 40, 100), 48),
            NavPosition::Top
        );
    }

    #[test]
    fn auto_picks_the_configured_wide_and_narrow_positions() {
        let setting = NavPositionSetting {
            wide: NavPosition::Right,
            narrow: NavPosition::Bottom,
            ..NavPositionSetting::default()
        };
        assert_eq!(
            resolve_nav_position(&setting, None, Rect::new(0, 0, 140, 30), 48),
            NavPosition::Right
        );
        assert_eq!(
            resolve_nav_position(&setting, None, Rect::new(0, 0, 40, 100), 48),
            NavPosition::Bottom
        );
    }

    #[test]
    fn with_auto_off_the_force_wins_and_wide_is_the_fallback() {
        let forced = NavPositionSetting {
            auto: false,
            force: Some(NavPosition::Bottom),
            ..NavPositionSetting::default()
        };
        assert_eq!(
            resolve_nav_position(&forced, None, Rect::new(0, 0, 40, 100), 48),
            NavPosition::Bottom,
            "force applies whatever the aspect"
        );
        let unforced = NavPositionSetting {
            auto: false,
            ..NavPositionSetting::default()
        };
        assert_eq!(
            resolve_nav_position(&unforced, None, Rect::new(0, 0, 140, 30), 48),
            NavPosition::Left,
            "no force falls back to the wide position"
        );
    }

    #[test]
    fn the_default_settings_reproduce_the_default_placements() {
        let setting = NavPositionSetting::default();
        assert!(setting.auto);
        assert_eq!(setting.wide, NavPosition::Left);
        assert_eq!(setting.narrow, NavPosition::Top);
        assert_eq!(setting.force, None);
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
