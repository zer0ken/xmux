//! Column-flow geometry for the portrait `Top` nav: pure, backend-free placement of
//! navigation cards into columns that fill DOWNWARD and then continue to the RIGHT.
//!
//! The `Top` nav is a wide, short band, so one vertical list would show three or four
//! cards and waste the rest of the row. Cards therefore stack down a column until the
//! next host/mux RUN would not fit, and that run starts the next column: a column holds
//! whole runs, so a source's cards are never split across a column break and the run's
//! first card - the only one carrying the `{host}/{mux}` context line - always sits at
//! the top of its own group. Reading order is the fill order: down a column, then right.
//!
//! The one exception is a run taller than the whole column, which has nowhere else to
//! go: it splits, and the continuation card re-states its context (it is a column's
//! first card, so it cannot hang under a context line that is now in another column).

use ratatui::layout::Rect;

/// A card as the flow needs to see it: where a run starts and how wide each of its two
/// lines renders, so the flow can size a column without knowing how a card is painted.
pub(super) struct Card {
    /// True when this card opens a new host/mux run: it always keeps its context line.
    pub(super) starts_run: bool,
    /// Display width of the context line (`{host}/{mux}`), address column included.
    pub(super) ctx_w: u16,
    /// Display width of the detail line (`{session}/{window}`), address column included.
    pub(super) detail_w: u16,
    /// The card's natural line count: two for a session card, one for a single host
    /// row (a reachable empty host). The flow measures a card from this, so a
    /// one-line card occupies one row instead of a row and a blank.
    pub(super) lines: u16,
}

impl Card {
    /// The width this card occupies: both lines when it keeps its context, the detail
    /// line alone when it collapses under the card above.
    fn width(&self, collapsed: bool) -> u16 {
        if collapsed {
            self.detail_w
        } else {
            self.ctx_w.max(self.detail_w)
        }
    }
}

/// Where the flow put one card.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Placed {
    /// 0-based column, counted from the left of the whole flow (not of the visible part).
    pub(super) col: usize,
    /// Rows from the top of the column.
    pub(super) y: u16,
    pub(super) h: u16,
    /// True when the card drops its context line (it hangs under the card above).
    pub(super) collapsed: bool,
}

/// One placed card's screen rect, paired with its card index.
pub(super) struct Cell {
    pub(super) idx: usize,
    pub(super) rect: Rect,
    pub(super) collapsed: bool,
}

/// Assigns every card a column and a row offset. `col_h` is the rows one column may
/// use; a column shorter than a full card renders every card collapsed (its detail
/// line) rather than clipping half of each, so a two-row band still names its sessions.
pub(super) fn place(cards: &[Card], col_h: u16) -> Vec<Placed> {
    let mut out: Vec<Placed> = Vec::with_capacity(cards.len());
    if cards.is_empty() || col_h == 0 {
        return out;
    }
    let squat = col_h < 2; // no room for a context line anywhere
    let mut col = 0usize;
    let mut used = 0u16;
    let mut i = 0usize;
    while i < cards.len() {
        // The run: this card and every following one that hangs under it.
        let mut j = i + 1;
        while j < cards.len() && !cards[j].starts_run {
            j += 1;
        }
        let run_h = if squat {
            (j - i) as u16
        } else {
            2 + (j - i - 1) as u16
        };
        // A run that would overflow this column starts the next one - unless the column
        // is empty, where there is no next column to gain anything by.
        if used > 0 && used + run_h > col_h {
            col += 1;
            used = 0;
        }
        for k in i..j {
            let mut collapsed = squat || (k > i && used > 0);
            // A card's natural height is its line count; collapsing it to its detail
            // line shrinks a two-line card to one (a one-line card is already as short
            // as it gets).
            let mut h = if collapsed { 1 } else { cards[k].lines };
            if used > 0 && used + h > col_h {
                // Only reachable for a run taller than a whole column: it splits, and
                // the continuation opens a column, so it states its context again.
                col += 1;
                used = 0;
                collapsed = squat;
                h = if collapsed { 1 } else { cards[k].lines };
            }
            out.push(Placed {
                col,
                y: used,
                h,
                collapsed,
            });
            used += h;
        }
        i = j;
    }
    out
}

/// Each column's width: the widest card it holds, capped at the area width so one long
/// name cannot push a column past the nav.
pub(super) fn widths(cards: &[Card], placed: &[Placed], max_w: u16) -> Vec<u16> {
    let cols = placed.iter().map(|p| p.col).max().map_or(0, |c| c + 1);
    let mut w = vec![0u16; cols];
    for (c, p) in cards.iter().zip(placed) {
        w[p.col] = w[p.col].max(c.width(p.collapsed).min(max_w));
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
pub(super) fn scroll_to(
    widths: &[u16],
    area_w: u16,
    gutter: u16,
    offset: usize,
    sel_col: usize,
) -> usize {
    let mut first = offset.min(widths.len().saturating_sub(1));
    if sel_col < first {
        return sel_col;
    }
    while sel_col >= first + visible_cols(widths, area_w, first, gutter).max(1) {
        first += 1;
    }
    first
}

/// How many cards sit in the columns OFF SCREEN either side of the window that starts at
/// `first` and holds `shown` columns: `(left, right)`. Cards, not columns, because a count
/// of columns answers a question about the layout while the reader is asking one about
/// their sessions.
pub(super) fn hidden_counts(placed: &[Placed], first: usize, shown: usize) -> (usize, usize) {
    let last = first + shown; // exclusive
    let left = placed.iter().filter(|p| p.col < first).count();
    let right = placed.iter().filter(|p| p.col >= last).count();
    (left, right)
}

/// Turns placements into screen rects for the columns starting at `first`, in `area`.
/// Cards in columns left of `first` or past the right edge get no cell, so they are
/// neither painted nor clickable.
pub(super) fn cells(
    placed: &[Placed],
    widths: &[u16],
    area: Rect,
    first: usize,
    gutter: u16,
) -> Vec<Cell> {
    let shown = visible_cols(widths, area.width, first, gutter);
    let mut x = vec![0u16; widths.len()];
    let mut cur = 0u16;
    for i in first..(first + shown).min(widths.len()) {
        if i > first {
            cur += gutter;
        }
        x[i] = cur;
        cur += widths[i];
    }
    let mut out = Vec::new();
    for (idx, p) in placed.iter().enumerate() {
        if p.col < first || p.col >= first + shown {
            continue;
        }
        // The last drawn column is clipped to the area edge when it alone is too wide.
        let w = widths[p.col].min(area.width.saturating_sub(x[p.col]));
        if w == 0 || p.y >= area.height {
            continue;
        }
        out.push(Cell {
            idx,
            rect: Rect {
                x: area.x + x[p.col],
                y: area.y + p.y,
                width: w,
                height: p.h.min(area.height - p.y),
            },
            collapsed: p.collapsed,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` cards in one run: the first opens it, the rest hang under it.
    fn run(n: usize, ctx_w: u16, detail_w: u16) -> Vec<Card> {
        (0..n)
            .map(|k| Card {
                starts_run: k == 0,
                ctx_w,
                detail_w,
                lines: 2,
            })
            .collect()
    }

    #[test]
    fn a_run_fills_downward_then_the_next_run_starts_a_column() {
        // Two runs of 3 (4 rows each: one expanded card + two collapsed) in an 8-row
        // column: both fit, stacked in ONE column.
        let mut cards = run(3, 10, 12);
        cards.extend(run(3, 10, 12));
        let p = place(&cards, 8);
        assert!(
            p.iter().all(|c| c.col == 0),
            "8 rows hold both 4-row runs: {p:?}"
        );
        assert_eq!(
            p.iter().map(|c| c.y).collect::<Vec<_>>(),
            vec![0, 2, 3, 4, 6, 7],
            "cards stack down the column"
        );
        // One row less and the second run cannot fit, so it opens the next column
        // whole - the first run is never split to fill the gap.
        let p = place(&cards, 7);
        assert_eq!(
            p.iter().map(|c| c.col).collect::<Vec<_>>(),
            vec![0, 0, 0, 1, 1, 1],
            "the run that does not fit moves right entire: {p:?}"
        );
        assert_eq!(p[3].y, 0, "and starts at the top of its column");
        assert!(!p[3].collapsed, "a column's first card keeps its context");
    }

    #[test]
    fn a_run_taller_than_the_column_splits_and_restates_its_context() {
        // A 6-card run needs 7 rows; the column has 4. It has nowhere to go but across.
        let p = place(&run(6, 10, 12), 4);
        assert_eq!(
            p.iter().map(|c| c.col).collect::<Vec<_>>(),
            vec![0, 0, 0, 1, 1, 1],
            "the run splits at the column edge: {p:?}"
        );
        assert!(
            !p[3].collapsed,
            "the continuation opens a column, so it re-states its context"
        );
        assert_eq!(p[3].y, 0);
    }

    #[test]
    fn a_two_row_band_keeps_one_card_per_column() {
        let p = place(&run(3, 10, 12), 2);
        assert_eq!(p.iter().map(|c| c.col).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(p.iter().all(|c| c.h == 2 && !c.collapsed));
    }

    #[test]
    fn a_one_row_band_collapses_every_card() {
        // No room for a context line at all: every card shows its detail line, so a
        // sliver of a nav still names its sessions instead of clipping each in half.
        let p = place(&run(3, 10, 12), 1);
        assert_eq!(p.iter().map(|c| c.col).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(p.iter().all(|c| c.h == 1 && c.collapsed));
    }

    #[test]
    fn a_column_is_as_wide_as_its_widest_card() {
        let cards = vec![
            Card {
                starts_run: true,
                ctx_w: 8,
                detail_w: 20,
                lines: 2,
            },
            Card {
                starts_run: false,
                ctx_w: 8,
                detail_w: 12,
                lines: 2,
            },
        ];
        let p = place(&cards, 8);
        assert_eq!(widths(&cards, &p, 100), vec![20]);
        // A collapsed card is measured by its detail line alone; an expanded one by
        // whichever of its two lines is wider.
        let cards = vec![Card {
            starts_run: true,
            ctx_w: 30,
            detail_w: 12,
            lines: 2,
        }];
        assert_eq!(widths(&cards, &place(&cards, 8), 100), vec![30]);
        assert_eq!(
            widths(&cards, &place(&cards, 8), 20),
            vec![20],
            "capped at the nav width"
        );
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
        assert_eq!(scroll_to(&w, 21, 1, 0, 2), 1, "column 2 needs offset 1");
        assert_eq!(scroll_to(&w, 21, 1, 0, 1), 0, "column 1 is already visible");
        assert_eq!(scroll_to(&w, 21, 1, 2, 0), 0, "scrolls back left");
    }

    #[test]
    fn the_hidden_counts_are_cards_either_side_of_the_window() {
        // Three columns of two cards. With one column on screen, the count either side is
        // in CARDS: what the reader is looking for is a session, not a column.
        let cards = run(2, 10, 10);
        let mut all = cards;
        all.extend(run(2, 10, 10));
        all.extend(run(2, 10, 10));
        let p = place(&all, 3); // one run per column
        assert_eq!(
            hidden_counts(&p, 0, 1),
            (0, 4),
            "two columns hide to the right"
        );
        assert_eq!(hidden_counts(&p, 1, 1), (2, 2), "one either side");
        assert_eq!(hidden_counts(&p, 2, 1), (4, 0), "all of them to the left");
        assert_eq!(
            hidden_counts(&p, 0, 3),
            (0, 0),
            "nothing hidden, nothing counted"
        );
    }

    #[test]
    fn cells_place_columns_left_to_right_with_a_gutter() {
        let cards = run(2, 10, 10);
        let p = place(&cards, 2); // one card per column
        let w = widths(&cards, &p, 100);
        let c = cells(&p, &w, Rect::new(5, 3, 21, 2), 0, 1);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].rect.x, c[0].rect.y), (5, 3));
        assert_eq!(
            (c[1].rect.x, c[1].rect.y),
            (16, 3),
            "next column starts past the first plus the gutter"
        );
        assert!(c.iter().all(|c| c.rect.width == 10 && c.rect.height == 2));
        // Scrolled one column right: the first column is neither painted nor clickable.
        let c = cells(&p, &w, Rect::new(5, 3, 21, 2), 1, 1);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].idx, 1);
        assert_eq!(c[0].rect.x, 5, "the drawn column sits at the left edge");
    }
}
