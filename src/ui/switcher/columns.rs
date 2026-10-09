//! Column-flow geometry for the portrait `Top` nav: pure, backend-free placement of
//! the nav's rows into columns that fill DOWNWARD and then continue to the RIGHT,
//! plus whitespace between session, no-session and disconnected-host groups.
//!
//! The `Top` nav is a wide, short band, so one vertical list would show three or four
//! cards and waste the rest of the row. Rows therefore stack down a column until the
//! next unit would not fit, and that unit starts the next column: a column holds whole
//! sections (a `{machine}/{mux}` title over its session cards), so a host's cards are
//! never split across a column break and the title naming them stays at the top of
//! them. Reading order is the fill order: down a column, then right.
//!
//! The one exception is a section taller than the whole column, which has nowhere else
//! to go: it splits, and the continuation starts at the top of the next column.
//! The title appears only where the section starts. Reading order connects the
//! remaining cards to that section.
//!
//! The host-state cards are a band of their own, never sharing a column with session
//! cards. Every group starts a fresh column, with one character of space between
//! columns. The same spacing applies when the band scrolls.

use ratatui::layout::Rect;

/// A card as the flow needs to see it: where a section run starts, and how wide and
/// tall it renders.
pub(super) struct Card {
    /// Starts the disconnected-host group after the no-session group.
    pub(super) separates_group: bool,
    /// True when this card opens a new unit: a section title (its session cards hang
    /// under it) or a host-state card. A session card is false and hangs under its
    /// section.
    pub(super) starts_run: bool,
    /// Display width of the card's content (address column included). A section title's
    /// width is its `{machine}/{mux}` alone: in the band a title carries no trailing rule,
    /// so what it measures is what it paints.
    pub(super) width: u16,
    /// The card's natural line count (1, or 2 for a scanning host card).
    pub(super) lines: u16,
}

/// Where the flow put one card.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Placed {
    /// 0-based column, counted from the left of the whole flow (not of the visible part).
    pub(super) col: usize,
    /// Rows from the top of the column.
    pub(super) y: u16,
    pub(super) h: u16,
}

/// One placed card's screen rect, paired with its card index.
pub(super) struct Cell {
    pub(super) idx: usize,
    pub(super) rect: Rect,
}

/// Assigns every card a column and a row offset. `boundary` is the index of the first
/// host-state card; the host band it opens never shares a column with session cards.
/// A section taller than a whole column splits, with its remaining cards starting
/// at the top of each following column. The section title appears only once.
pub(super) fn place(cards: &[Card], col_h: u16, boundary: usize) -> Vec<Placed> {
    let mut out: Vec<Placed> = Vec::with_capacity(cards.len());
    if cards.is_empty() || col_h == 0 {
        return out;
    }
    let mut col = 0usize;
    let mut used = 0u16;
    let mut i = 0usize;
    while i < cards.len() {
        // The host band never shares a column with session cards: the first host card
        // opens a fresh column of its own.
        if (i == boundary || cards[i].separates_group) && used > 0 {
            col += 1;
            used = 0;
        }
        // The run: this card and every session card hanging under it.
        let mut j = i + 1;
        while j < cards.len() && !cards[j].starts_run {
            j += 1;
        }
        let run_h: u16 = cards[i..j].iter().map(|c| c.lines).sum();
        // A run that would overflow this column starts the next one - unless the column
        // is empty, where there is no next column to gain anything by.
        if used > 0 && used + run_h > col_h {
            col += 1;
            used = 0;
        }
        for card in cards.iter().take(j).skip(i) {
            let h = card.lines.min(col_h);
            if h == 0 {
                continue;
            }
            if used > 0 && used + h > col_h {
                // Only reachable for a run taller than a whole column: it splits, and
                // the continuation opens at the top of the next column.
                col += 1;
                used = 0;
            }
            out.push(Placed { col, y: used, h });
            used += h;
        }
        i = j;
    }
    out
}

/// Each column's width is its widest card, capped at the nav width.
pub(super) fn widths(cards: &[Card], placed: &[Placed], max_w: u16) -> Vec<u16> {
    let cols = placed.iter().map(|p| p.col).max().map_or(0, |c| c + 1);
    let mut w = vec![0u16; cols];
    for (c, p) in cards.iter().zip(placed) {
        w[p.col] = w[p.col].max(c.width.min(max_w));
    }
    w
}

/// How many columns starting at `first` fit in `area_w`, counting a column only when it
/// fits WHOLE (a half-drawn card reads as a shorter name). The first drawn column is
/// always counted: something must show even when it alone is wider than the nav.
pub(super) fn visible_cols(widths: &[u16], area_w: u16, first: usize, gutter: u16) -> usize {
    let mut n = 0usize;
    let mut x = 0u16;
    for (i, w) in widths.iter().enumerate().skip(first) {
        let gap = if i == first { 0 } else { gutter };
        if x + gap + w > area_w && n > 0 {
            break;
        }
        x += gap + w;
        n += 1;
    }
    n
}

/// The first column to draw so the column holding `sel_col` is visible, given the
/// current `offset`. Scrolls the minimum distance: left to the selected column when it
/// is left of the window, right one column at a time until it is inside.
///
/// `title_col` is the column the selected card's section starts in, `None` when the
/// card hangs under no section. A section taller than a column splits, and its title
/// stays where the section starts: when the columns from the title through the card
/// fit together, the window shows the title instead of leaving it left of the edge.
pub(super) fn scroll_to(
    widths: &[u16],
    area_w: u16,
    gutter: u16,
    offset: usize,
    sel_col: usize,
    title_col: Option<usize>,
) -> usize {
    if visible_cols(widths, area_w, 0, gutter) == widths.len() {
        return 0;
    }
    let mut first = offset.min(widths.len().saturating_sub(1));
    if sel_col < first {
        first = sel_col;
    }
    while sel_col >= first + visible_cols(widths, area_w, first, gutter).max(1) {
        first += 1;
    }
    if let Some(t) = title_col {
        if t < first && span_cols(widths, gutter, t, sel_col) <= area_w {
            first = t;
        }
    }
    first
}

/// The width from column `from` through column `to`, counting the gutter between
/// columns: what the window has to hold to show both columns at once.
fn span_cols(widths: &[u16], gutter: u16, from: usize, to: usize) -> u16 {
    let mut x = 0u16;
    for (i, w) in widths.iter().enumerate().take(to + 1).skip(from) {
        let gap = if i == from { 0 } else { gutter };
        x += gap + w;
    }
    x
}

/// How many cards sit in the columns OFF SCREEN either side of the window that starts at
/// `first` and holds `shown` columns: `(left, right)`. Cards, not columns, because a count
/// of columns answers a question about the layout while the reader is asking one about
/// their sessions; and cards, not rows, so a section title, which `is_card` rejects, is
/// never counted.
pub(super) fn hidden_counts(
    placed: &[Placed],
    first: usize,
    shown: usize,
    is_card: impl Fn(usize) -> bool,
) -> (usize, usize) {
    let last = first + shown; // exclusive
    let count = |hidden: &dyn Fn(usize) -> bool| {
        placed
            .iter()
            .enumerate()
            .filter(|(i, p)| hidden(p.col) && is_card(*i))
            .count()
    };
    (count(&|c| c < first), count(&|c| c >= last))
}

/// Places visible columns with the same gutter at every group boundary.
/// Offscreen cards have no painted or clickable rectangle.
pub(super) fn cells(
    placed: &[Placed],
    widths: &[u16],
    area: Rect,
    first: usize,
    gutter: u16,
) -> Vec<Cell> {
    let dw = widths;
    let shown = visible_cols(dw, area.width, first, gutter);
    let mut x = vec![0u16; dw.len()];
    let mut cur = 0u16;
    for i in first..(first + shown).min(dw.len()) {
        if i > first {
            cur += gutter;
        }
        x[i] = cur;
        cur += dw[i];
    }
    let mut out = Vec::new();
    for (idx, p) in placed.iter().enumerate() {
        let dcol = p.col;
        if dcol < first || dcol >= first + shown {
            continue;
        }
        // The last drawn column is clipped to the area edge when it alone is too wide.
        let w = dw[dcol].min(area.width.saturating_sub(x[dcol]));
        if w == 0 || p.y >= area.height {
            continue;
        }
        out.push(Cell {
            idx,
            rect: Rect {
                x: area.x + x[dcol],
                y: area.y + p.y,
                width: w,
                height: p.h.min(area.height - p.y),
            },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` cards in one section run: a title over `n - 1` hanging session cards.
    fn run(n: usize, width: u16) -> Vec<Card> {
        (0..n)
            .map(|k| Card {
                separates_group: false,
                starts_run: k == 0,
                width,
                lines: 1,
            })
            .collect()
    }

    /// A lone host-state card.
    fn host(width: u16, lines: u16) -> Vec<Card> {
        vec![Card {
            separates_group: false,
            starts_run: true,
            width,
            lines,
        }]
    }

    /// The rows and columns the placement gave each card.
    fn ys(placed: &[Placed]) -> Vec<(usize, u16, u16)> {
        placed.iter().map(|p| (p.col, p.y, p.h)).collect()
    }

    #[test]
    fn a_section_fills_downward_then_the_next_section_starts_a_column() {
        // Two sections of 3 (4 rows each: a title + two sessions) in a 6-row column:
        // both fit, stacked in ONE column.
        let mut cards = run(3, 10);
        cards.extend(run(3, 10));
        let p = place(&cards, 6, cards.len());
        assert!(
            p.iter().all(|c| c.col == 0),
            "6 rows hold both 3-row sections: {p:?}"
        );
        assert_eq!(
            ys(&p),
            vec![
                (0, 0, 1),
                (0, 1, 1),
                (0, 2, 1),
                (0, 3, 1),
                (0, 4, 1),
                (0, 5, 1)
            ],
            "cards stack down the column; the second title leads at row 3"
        );
        // One row less and the second section cannot fit, so it opens the next column
        // whole - the first section is never split to fill the gap.
        let p = place(&cards, 5, cards.len());
        assert_eq!(
            p.iter().map(|c| c.col).collect::<Vec<_>>(),
            vec![0, 0, 0, 1, 1, 1],
            "the section that does not fit moves right entire: {p:?}"
        );
        assert_eq!(p[3].y, 0, "and starts at the top of its column");
    }

    #[test]
    fn a_section_taller_than_the_column_starts_continuations_at_the_top() {
        let mut cards = run(10, 6);
        cards[0].width = 20;
        let p = place(&cards, 4, cards.len());
        assert_eq!(
            ys(&p),
            vec![
                (0, 0, 1),
                (0, 1, 1),
                (0, 2, 1),
                (0, 3, 1),
                (1, 0, 1),
                (1, 1, 1),
                (1, 2, 1),
                (1, 3, 1),
                (2, 0, 1),
                (2, 1, 1),
            ]
        );
        assert_eq!(widths(&cards, &p, 100), vec![20, 6, 6]);
    }

    #[test]
    fn a_two_row_band_fills_continuations_with_cards() {
        let cards = run(5, 10);
        let p = place(&cards, 2, cards.len());
        assert_eq!(
            ys(&p),
            vec![(0, 0, 1), (0, 1, 1), (1, 0, 1), (1, 1, 1), (2, 0, 1)]
        );
    }

    #[test]
    fn a_one_row_band_runs_every_card_along_its_row() {
        let cards = run(3, 10);
        let p = place(&cards, 1, cards.len());
        assert_eq!(ys(&p), vec![(0, 0, 1), (1, 0, 1), (2, 0, 1)]);
    }

    #[test]
    fn the_host_band_never_shares_a_column_with_sessions() {
        // Two session sections plus one host card in a 3-row band: the host card would
        // fit beside the sessions, but it must not - it opens a band of its own.
        let mut cards = run(2, 8);
        cards.extend(run(2, 8));
        let boundary = cards.len();
        cards.extend(host(8, 1));
        let p = place(&cards, 3, boundary);
        assert_eq!(
            p.iter().map(|c| c.col).collect::<Vec<_>>(),
            vec![0, 0, 1, 1, 2],
            "sections fill columns, the host card opens its own"
        );
    }

    #[test]
    fn a_column_is_as_wide_as_its_widest_card() {
        let cards = vec![
            Card {
                separates_group: false,
                starts_run: true,
                width: 8,
                lines: 1,
            },
            Card {
                separates_group: false,
                starts_run: false,
                width: 20,
                lines: 1,
            },
        ];
        let p = place(&cards, 8, cards.len());
        assert_eq!(widths(&cards, &p, 100), vec![20]);
        assert_eq!(widths(&cards, &p, 12), vec![12], "capped at the nav width");
    }

    #[test]
    fn group_boundaries_keep_one_gutter_at_every_width() {
        let mut cards = run(2, 10);
        let boundary = cards.len();
        cards.extend(host(10, 1));
        let mut disconnected = host(10, 1);
        disconnected[0].separates_group = true;
        cards.extend(disconnected);
        let placed = place(&cards, 3, boundary);
        let widths = widths(&cards, &placed, 100);
        assert_eq!(widths, vec![10, 10, 10]);
        for width in [20, 21, 31, 32, 60] {
            for selected in 0..3 {
                let first = scroll_to(&widths, width, 1, 0, selected, None);
                let visible = cells(&placed, &widths, Rect::new(0, 0, width, 3), first, 1);
                let selected_card = [0, 2, 3][selected];
                assert!(visible.iter().any(|cell| cell.idx == selected_card));
                let mut columns: Vec<Rect> = visible.iter().map(|cell| cell.rect).collect();
                columns.dedup_by_key(|rect| rect.x);
                for pair in columns.windows(2) {
                    assert_eq!(pair[1].x - pair[0].right(), 1);
                }
            }
        }
    }

    #[test]
    fn only_whole_columns_are_drawn_and_the_selection_stays_visible() {
        let w = vec![10, 10, 10];
        // 21 columns of room holds two 10-wide columns plus the 1-cell gutter.
        assert_eq!(visible_cols(&w, 21, 0, 1), 2);
        assert_eq!(visible_cols(&w, 20, 0, 1), 1, "no room for a whole second");
        assert_eq!(
            visible_cols(&[30], 10, 0, 1),
            1,
            "the first drawn column always shows, clipped"
        );
        // Selecting a card in a column right of the window scrolls just far enough.
        assert_eq!(
            scroll_to(&w, 21, 1, 0, 2, None),
            1,
            "column 2 needs offset 1"
        );
        assert_eq!(
            scroll_to(&w, 21, 1, 0, 1, None),
            0,
            "column 1 is already visible"
        );
        assert_eq!(scroll_to(&w, 21, 1, 2, 0, None), 0, "scrolls back left");
        assert_eq!(
            scroll_to(&w, 32, 1, 2, 2, None),
            0,
            "all columns fit after widening"
        );
        // The selected card's section title pulls the window left to it when the
        // columns from the title through the card fit together; a span wider than the
        // band leaves the card where it is, since the title would hide it.
        assert_eq!(
            scroll_to(&[10, 10, 10, 10], 45, 1, 3, 3, Some(0)),
            0,
            "the title through the card fits, the window shows the title"
        );
        assert_eq!(
            scroll_to(&[30, 30, 10], 45, 1, 2, 2, Some(0)),
            2,
            "the span does not fit, the card keeps the position that shows it"
        );
    }

    #[test]
    fn the_hidden_counts_are_cards_either_side_of_the_window() {
        // Three columns of a title over two cards. With one column on screen, the count
        // either side is in CARDS: what the reader is looking for is a session, not a
        // column, and a title is not a card.
        let mut all = run(3, 10);
        all.extend(run(3, 10));
        all.extend(run(3, 10));
        let p = place(&all, 3, all.len()); // one section per column
        let card = |i: usize| !i.is_multiple_of(3);
        assert_eq!(
            hidden_counts(&p, 0, 1, card),
            (0, 4),
            "two columns hide to the right"
        );
        assert_eq!(hidden_counts(&p, 1, 1, card), (2, 2), "one either side");
        assert_eq!(
            hidden_counts(&p, 2, 1, card),
            (4, 0),
            "all of them to the left"
        );
        assert_eq!(hidden_counts(&p, 0, 3, card), (0, 0), "nothing hidden");
    }

    #[test]
    fn cells_place_columns_left_to_right_with_a_gutter() {
        let cards = run(2, 10);
        let p = place(&cards, 1, cards.len()); // one card per column
        let w = widths(&cards, &p, 100);
        let first_cells = cells(&p, &w, Rect::new(5, 3, 21, 2), 0, 1);
        assert_eq!(first_cells.len(), 2);
        assert_eq!((first_cells[0].rect.x, first_cells[0].rect.y), (5, 3));
        assert_eq!(
            (first_cells[1].rect.x, first_cells[1].rect.y),
            (16, 3),
            "next column starts past the first plus the gutter"
        );
        // Scrolled one column right: the first column is neither painted nor clickable.
        let scrolled = cells(&p, &w, Rect::new(5, 3, 21, 2), 1, 1);
        assert_eq!(scrolled.len(), 1);
        assert_eq!(scrolled[0].idx, 1);
        assert_eq!(
            scrolled[0].rect.x, 5,
            "the drawn column sits at the left edge"
        );
    }
}
