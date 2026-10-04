//! Off-screen rendering helpers for control-channel screen dumps.

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::ui::switcher::Switcher;

/// Renders the switcher to an off-screen buffer and flattens it as the control
/// channel's `dump` payload.
pub fn dump_switcher(
    switcher: &Switcher,
    state: &crate::state::State,
    width: u16,
    height: u16,
) -> String {
    dump_screen(
        switcher,
        None,
        width,
        height,
        state,
        &crate::ui::switcher::RenderPlan::default(),
    )
}

/// Renders the nav-focused view with the selected host's live grid, when one
/// exists, to an off-screen backend and flattens it. A headless `dump` therefore
/// reflects the same screen the main draw produces, including the live terminal
/// grid, without a real terminal. `previous` is the plan of the last drawn frame, so
/// the dump lays out from the same scroll offsets as the screen.
pub fn dump_screen(
    switcher: &Switcher,
    grid: Option<&crate::display::grid::Grid>,
    width: u16,
    height: u16,
    state: &crate::state::State,
    previous: &crate::ui::switcher::RenderPlan,
) -> String {
    let w = width.max(1);
    let h = height.max(1);
    let mut term = match Terminal::new(TestBackend::new(w, h)) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    if term
        .draw(|f| {
            let nav = if previous.screen_area.is_empty() {
                crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH)
            } else {
                previous.nav_size
            };
            let plan = switcher.layout(f.area(), nav, state, previous);
            switcher.render(f, grid, state.focus.is_terminal_focused(), state, &plan)
        })
        .is_err()
    {
        return String::new();
    }
    flatten_buffer(term.backend().buffer())
}

/// Flattens a rendered buffer to text (one trimmed line per row).
fn flatten_buffer(buf: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::state::Scan;
    use crate::ui::tree::Group;

    #[test]
    fn scanning_dump_uses_the_same_braille_frame_as_live_render() {
        let mut state = crate::state::State::from_sources(vec!["pending".into()]);
        let switcher = Switcher::from_sources(&mut state);
        state.chrome.animation_ms = 1_066;
        for (width, height, expected_columns) in [(90, 24, 32), (130, 40, 64)] {
            let previous = crate::ui::switcher::RenderPlan::default();
            let dumped = dump_screen(&switcher, None, width, height, &state, &previous);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    let nav = crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH);
                    let plan = switcher.layout(frame.area(), nav, &state, &previous);
                    assert_eq!(plan.view_screen, Some(crate::model::ViewScreen::Scanning));
                    switcher.render(frame, None, false, &state, &plan);
                })
                .unwrap();
            assert_eq!(dumped, flatten_buffer(terminal.backend().buffer()));
            assert!(dumped.lines().any(|line| {
                line.chars()
                    .filter(|c| ('\u{2800}'..='\u{28ff}').contains(c))
                    .count()
                    >= expected_columns
            }));
        }
    }

    #[test]
    fn scanning_dump_advances_and_small_view_clips_safely() {
        let mut state = crate::state::State::from_sources(vec!["pending".into()]);
        let switcher = Switcher::from_sources(&mut state);
        let previous = crate::ui::switcher::RenderPlan::default();
        let first = dump_screen(&switcher, None, 70, 24, &state, &previous);
        state.chrome.animation_ms = 1_132;
        let turned = dump_screen(&switcher, None, 70, 24, &state, &previous);
        assert_ne!(first, turned);
        state.chrome.animation_ms = 0;
        let small = dump_screen(&switcher, None, 55, 8, &state, &previous);
        assert_eq!(small.lines().count(), 8);
        assert!(small
            .chars()
            .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));
    }

    #[test]
    fn another_hosts_scan_does_not_cover_the_selected_session() {
        let mut state = crate::state::State::from_scan(sample());
        let switcher = Switcher::new(&mut state);
        state.scanning.insert("other".into());
        let previous = crate::ui::switcher::RenderPlan::default();
        let plan = switcher.layout(
            ratatui::layout::Rect::new(0, 0, 100, 30),
            crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
            &state,
            &previous,
        );
        assert_eq!(plan.view_screen, None);
    }

    #[test]
    fn scanning_screen_does_not_inherit_a_stale_grid_cursor() {
        let mut state = crate::state::State::from_sources(vec!["pending".into()]);
        let switcher = Switcher::from_sources(&mut state);
        let mut grid = crate::display::grid::Grid::new(50, 30);
        grid.feed(b"old session");
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| {
                let plan = switcher.layout(
                    frame.area(),
                    crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
                    &state,
                    &crate::ui::switcher::RenderPlan::default(),
                );
                assert_eq!(plan.view_screen, Some(crate::model::ViewScreen::Scanning));
                switcher.render(frame, Some(&grid), true, &state, &plan);
            })
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn scanning_dump_matches_a_hidden_nav_frame() {
        let mut state = crate::state::State::from_sources(vec!["pending".into()]);
        let switcher = Switcher::from_sources(&mut state);
        state.chrome.animation_ms = 1_099;
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        let nav = crate::ui::switcher::NavSize::hidden(crate::ui::switcher::NAV_WIDTH);
        let previous = switcher.layout(area, nav, &state, &Default::default());
        let dumped = dump_screen(&switcher, None, area.width, area.height, &state, &previous);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                let plan = switcher.layout(frame.area(), nav, &state, &previous);
                switcher.render(frame, None, false, &state, &plan);
            })
            .unwrap();
        assert_eq!(dumped, flatten_buffer(terminal.backend().buffer()));
        assert!(dumped
            .chars()
            .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)));
    }

    fn sample() -> Scan {
        Scan {
            groups: vec![Group {
                source: "local".into(),
                err: None,
                sessions: vec![Session {
                    source: "local".into(),
                    name: "editor".into(),
                    mux: "tmux".into(),
                    windows: 1,
                    attached: false,
                }],
            }],
        }
    }

    #[tokio::test]
    async fn dump_switcher_flattens_buffer() {
        let mut state = crate::state::State::from_scan(sample());
        let sw = Switcher::new(&mut state);
        let out = dump_switcher(&sw, &state, 100, 30);
        // The dump renders the full screen (tree and hint bar); at rest the bar shows the
        // prefix alone.
        assert!(out.contains("editor"));
        assert!(out.contains("C-g"), "hint bar prefix present:\n{out}");
    }

    #[tokio::test]
    async fn dump_screen_renders_the_live_grid() {
        let mut state = crate::state::State::from_scan(sample());
        let sw = Switcher::new(&mut state);
        let mut grid = crate::display::grid::Grid::new(30, 100);
        grid.feed(b"LIVEGRID");
        // A dump with a live grid includes both the tree and the grid content (the
        // terminal view), so a headless `dump` reflects the live grid.
        let out = dump_screen(
            &sw,
            Some(&grid),
            100,
            30,
            &state,
            &crate::ui::switcher::RenderPlan::default(),
        );
        assert!(out.contains("editor"), "tree still rendered:\n{out}");
        assert!(
            out.contains("LIVEGRID"),
            "live grid content rendered:\n{out}"
        );
    }
}
