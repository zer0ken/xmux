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
    dump_screen(switcher, None, width, height, state)
}

/// Renders the nav-focused view with the selected host's live grid, when one
/// exists, to an off-screen backend and flattens it.
pub fn dump_screen(
    switcher: &mut Switcher,
    grid: Option<&crate::display::grid::Grid>,
    width: u16,
    height: u16,
    state: &crate::state::State,
) -> String {
    let w = width.max(1);
    let h = height.max(1);
    let mut term = match Terminal::new(TestBackend::new(w, h)) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    if term
        .draw(|f| {
            switcher.render(
                f,
                grid,
                false,
                crate::ui::switcher::NavSize::visible(crate::ui::switcher::NAV_WIDTH),
                state,
            )
        })
        .is_err()
    {
        return String::new();
    }
    flatten_buffer(term.backend().buffer())
}

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
        assert!(out.contains("editor"));
        assert!(out.contains("C-g"), "hint bar prefix present:\n{out}");
    }

    #[tokio::test]
    async fn dump_screen_renders_the_live_grid() {
        let mut state = crate::state::State::from_scan(sample());
        let mut sw = Switcher::new(&mut state);
        let mut grid = crate::display::grid::Grid::new(30, 100);
        grid.feed(b"LIVEGRID");
        let out = dump_screen(&mut sw, Some(&grid), 100, 30, &state);
        assert!(out.contains("editor"), "tree still rendered:\n{out}");
        assert!(
            out.contains("LIVEGRID"),
            "live grid content rendered:\n{out}"
        );
    }
}
