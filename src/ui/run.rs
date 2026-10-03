//! Off-screen rendering helpers for control-channel screen dumps.

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::ui::switcher::Switcher;

/// Renders the switcher to an off-screen buffer and flattens it as the control
/// channel's `dump` payload.
pub fn dump_switcher(
    switcher: &mut Switcher,
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
    switcher: &mut Switcher,
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
            let nav = crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH);
            let plan = switcher.layout(f.area(), nav, state, previous);
            switcher.render(f, grid, false, state, &plan)
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
    use crate::ui::switcher::Scan;
    use crate::ui::tree::Group;

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
        let mut sw = Switcher::new(&mut state);
        let out = dump_switcher(&mut sw, &state, 100, 30);
        // The dump renders the full screen (tree and hint bar); at rest the bar shows the
        // prefix and collapse button.
        assert!(out.contains("editor"));
        assert!(out.contains("C-g"), "hint bar prefix present:\n{out}");
    }

    #[tokio::test]
    async fn dump_screen_renders_the_live_grid() {
        let mut state = crate::state::State::from_scan(sample());
        let mut sw = Switcher::new(&mut state);
        let mut grid = crate::display::grid::Grid::new(30, 100);
        grid.feed(b"LIVEGRID");
        // A dump with a live grid includes both the tree and the grid content (the
        // terminal view), so a headless `dump` reflects the live grid.
        let out = dump_screen(
            &mut sw,
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
