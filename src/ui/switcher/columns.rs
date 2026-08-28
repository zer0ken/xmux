//! Column-flow geometry for the portrait `Top` nav: pure, backend-free placement of
//! the nav's rows into columns that fill DOWNWARD and then continue to the RIGHT,
//! plus the parting of the two bands (the sessions left, the host-state cards right).
//!
//! The `Top` nav is a wide, short band, so one vertical list would show three or four
//! cards and waste the rest of the row. Rows therefore stack down a column until the
//! next unit would not fit, and that unit starts the next column: a column holds whole
//! sections (a `{host}/{mux}` title over its session cards), so a source's cards are
//! never split across a column break and the title naming them stays at the top of
//! them. Reading order is the fill order: down a column, then right.
//!
//! The one exception is a section taller than the whole column, which has nowhere else
//! to go: it splits, and the continuation re-states its title (it is a column's first
//! row, so it cannot hang under a title that is now in another column).
//!
//! The host-state cards are a band of their own, never sharing a column with session
//! cards. While the bands can spare a column for it they are pushed APART - sessions
//! against the left edge, host cards against the right - and the blank columns between
//! them are the parting, because a gap says "a different kind of thing follows"
//! without spending a glyph on saying it. Once they cannot, the band scrolls as one
//! run and a vertical rule takes the boundary's column instead. The parting therefore
//! always has a column of its own: the run is measured with the rule's column
//! included, so the bands go from a gap of one straight to a rule and never touch.

use ratatui::layout::Rect;

/// A card as the flow needs to see it: where a section run starts, and how wide and
/// tall it renders.
pub(super) struct Card {
    /// True when this card opens a new unit: a section title (its session cards hang
    /// under it) or a host-state card. A session card is false and hangs under its
    /// section.
    pub(super) starts_run: bool,
    /// Display width of the card's content (address column included). A section title's
    /// width is its `{host}/{mux}` alone: in the band a title carries no trailing rule,
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
    /// A section title re-stated just above this card, from the section card at this
    /// index: a section that split across a column break. Drawn, never clickable.
    pub(super) header: Option<usize>,
}

/// One placed card's screen rect, paired with its card index.
pub(super) struct Cell {
    pub(super) idx: usize,
    pub(super) rect: Rect,
    /// The re-stated section title to draw just above this cell's rect, if any.
    pub(super) header: Option<usize>,
}

/// How the two bands part in the horizontal direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Parting {
    /// Room to spare: the session columns hold the left edge and the host band is
    /// pushed to the right, blank columns between them.
    Gap,
    /// The bands would touch: a vertical rule column parts them, and the run scrolls.
    Rule,
}

/// Assigns every card a column and a row offset. `boundary` is the index of the first
/// host-state card; the host band it opens never shares a column with session cards.
/// A section taller than a whole column splits: the continuation opens a column and
/// re-states the section's title as a `header` on its first card.
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
        if i == boundary && used > 0 {
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
        for (k, card) in cards.iter().enumerate().take(j).skip(i) {
            let h = card.lines.min(col_h);
            if h == 0 {
                continue;
            }
            if used > 0 && used + h > col_h {
                // Only reachable for a run taller than a whole column: it splits, and
                // the continuation opens a column.
                col += 1;
                used = 0;
            }
            // A continuation (a session card that opens a column, never the section
            // title itself) re-states the section's title above it.
            let header = if k > i && used == 0 && col_h >= 2 {
                Some(i)
            } else {
                None
            };
            if header.is_some() {
                used = 1; // the re-stated title takes the column's first row
            }
            out.push(Placed {
                col,
                y: used,
                h,
                header,
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
        w[p.col] = w[p.col].max(c.width.min(max_w));
    }
    w
}

/// The column the host band begins at: the column of the first host-state card. When
/// there is no host band (`boundary` past the cards) the value is the flow's own
/// length, which reads as "past every session column".
pub(super) fn boundary_col(placed: &[Placed], boundary: usize) -> usize {
    if boundary == 0 || boundary >= placed.len() {
        boundary
    } else {
        placed[boundary].col
    }
}

/// How the two bands part, when both are present. The run is measured with the rule's
/// column included, so a gap of one is the last thing before a rule: the bands go from
/// a gap of one straight to a rule and never touch, at the price of scrolling one
/// column earlier than the cards alone would need.
pub(super) fn parting(
    widths: &[u16],
    boundary_col: usize,
    band_w: u16,
    gutter: u16,
) -> Option<Parting> {
    if boundary_col >= widths.len() {
        return None; // no host band: sessions alone flow from the left
    }
    if boundary_col == 0 {
        // The whole band is host-state cards (nothing has a session to show yet). Their
        // band is the ONLY band, so it anchors to the RIGHT edge and the blank columns
        // left of it are where the sessions that will be found land. If the host cards
        // alone overrun the band, they fall back to a plain flow from the left.
        let host = widths.iter().sum::<u16>() + (widths.len().saturating_sub(1) as u16) * gutter;
        return (host < band_w).then_some(Parting::Gap);
    }
    let sess = widths[..boundary_col].iter().sum::<u16>()
        + (boundary_col.saturating_sub(1) as u16) * gutter;
    let host = widths[boundary_col..].iter().sum::<u16>()
        + (widths.len().saturating_sub(boundary_col + 1) as u16) * gutter;
    if sess + host < band_w {
        // The rule's own column fits (the `+ 1` is implicit in the strict `<`), so the
        // bands part by a gap rather than a rule.
        Some(Parting::Gap)
    } else {
        Some(Parting::Rule)
    }
}

/// The display columns: the widths plus the band rule's own column when the bands are
/// parted by a rule. The rule sits at `boundary_col`, so the host columns shift one
/// right. In the gap parting the display is the widths as they are.
pub(super) fn display_widths(widths: &[u16], boundary_col: usize, parting: Parting) -> Vec<u16> {
    match parting {
        Parting::Gap => widths.to_vec(),
        Parting::Rule => {
            let mut w = Vec::with_capacity(widths.len() + 1);
            w.extend_from_slice(&widths[..boundary_col]);
            w.push(1); // the rule's own column
            w.extend_from_slice(&widths[boundary_col..]);
            w
        }
    }
}

/// A placed card's column in the display space, where the band rule (when it parts the
/// bands) has pushed the host columns one right.
pub(super) fn display_col(col: usize, boundary_col: usize, parting: Option<Parting>) -> usize {
    match parting {
        Some(Parting::Rule) if col >= boundary_col => col + 1,
        _ => col,
    }
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
pub(super) fn hidden_counts(
    placed: &[Placed],
    boundary_col: usize,
    parting: Option<Parting>,
    first: usize,
    shown: usize,
) -> (usize, usize) {
    let last = first + shown; // exclusive
    let left = placed
        .iter()
        .filter(|p| display_col(p.col, boundary_col, parting) < first)
        .count();
    let right = placed
        .iter()
        .filter(|p| display_col(p.col, boundary_col, parting) >= last)
        .count();
    (left, right)
}

/// Turns placements into screen rects for the visible columns, and returns the rect of
/// the band rule when the bands part by one. In the gap parting the host columns are
/// right-aligned against the band's right edge and nothing scrolls (everything fits by
/// construction); in the rule parting the whole run scrolls from the left like a plain
/// flow. Cards in columns left of `first` or past the right edge get no cell, so they
/// are neither painted nor clickable.
pub(super) fn cells(
    placed: &[Placed],
    widths: &[u16],
    boundary_col: usize,
    parting: Option<Parting>,
    area: Rect,
    first: usize,
    gutter: u16,
) -> (Vec<Cell>, Option<Rect>) {
    let dw = display_widths(widths, boundary_col, parting.unwrap_or(Parting::Gap));
    let shown = visible_cols(&dw, area.width, first, gutter);
    let mut x = vec![0u16; dw.len()];
    let mut cur = 0u16;
    for i in first..(first + shown).min(dw.len()) {
        if i > first {
            cur += gutter;
        }
        x[i] = cur;
        cur += dw[i];
    }
    // The gap parting: everything fits, so push the host columns against the band's
    // right edge, the blank columns between them being the parting.
    if parting == Some(Parting::Gap) {
        let host_total: u16 = widths[boundary_col..].iter().sum::<u16>()
            + (widths.len().saturating_sub(boundary_col + 1) as u16) * gutter;
        let host_start = area.width.saturating_sub(host_total);
        let mut cur = 0u16;
        for i in boundary_col..widths.len() {
            if i > boundary_col {
                cur += gutter;
            }
            x[i] = host_start + cur;
            cur += widths[i];
        }
    }
    // The rule column, when the bands part by one.
    let rule = match parting {
        Some(Parting::Rule) => Some(Rect {
            x: area.x + x[boundary_col],
            y: area.y,
            width: 1,
            height: area.height,
        }),
        _ => None,
    };
    let mut out = Vec::new();
    for (idx, p) in placed.iter().enumerate() {
        let dcol = display_col(p.col, boundary_col, parting);
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
            header: p.header,
        });
    }
    (out, rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` cards in one section run: a title over `n - 1` hanging session cards.
    fn run(n: usize, width: u16) -> Vec<Card> {
        (0..n)
            .map(|k| Card {
                starts_run: k == 0,
                width,
                lines: 1,
            })
            .collect()
    }

    /// A lone host-state card.
    fn host(width: u16, lines: u16) -> Vec<Card> {
        vec![Card {
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
    fn a_section_taller_than_the_column_splits_and_restates_its_title() {
        // A 6-card section needs 6 rows; the column has 4. It has nowhere to go but
        // across, and the continuation re-states the section's title.
        let cards = run(6, 10);
        let p = place(&cards, 4, cards.len());
        assert_eq!(
            p.iter().map(|c| c.col).collect::<Vec<_>>(),
            vec![0, 0, 0, 0, 1, 1],
            "the section splits at the column edge: {p:?}"
        );
        // The continuation's first card carries the re-stated title at the top of its
        // column, one row above it.
        assert_eq!(p[4].col, 1);
        assert_eq!(p[4].y, 1, "the title takes row 0, the card row 1");
        assert_eq!(
            p[4].header,
            Some(0),
            "the title is the section's own, re-stated"
        );
        assert_eq!(
            p[0].header, None,
            "the section's real title states nothing extra"
        );
        assert!(p[5].header.is_none());
    }

    #[test]
    fn a_two_row_band_splits_a_taller_section_with_a_restated_title() {
        // A 3-card section needs 3 rows; the column has 2. The continuation opens a
        // column and re-states the title above it.
        let cards = run(3, 10);
        let p = place(&cards, 2, cards.len());
        assert_eq!(
            p.iter().map(|c| c.col).collect::<Vec<_>>(),
            vec![0, 0, 1],
            "the title and one session share the first column"
        );
        assert_eq!(p[2].col, 1);
        assert_eq!(p[2].y, 1);
        assert_eq!(p[2].header, Some(0));
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
                starts_run: true,
                width: 8,
                lines: 1,
            },
            Card {
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
    fn the_gap_parts_the_bands_left_and_right() {
        // Sessions (one 10-wide column) plus a host card (10 wide) in a 30-wide band:
        // a rule column would fit, so the parting is a GAP and the host sits at the
        // right edge.
        let mut cards = run(2, 10);
        let boundary = cards.len();
        cards.extend(host(10, 1));
        let p = place(&cards, 3, boundary);
        let w = widths(&cards, &p, 100);
        assert_eq!(
            parting(&w, 1, 30, 1),
            Some(Parting::Gap),
            "sess 10 + host 10 + rule 1 fits in 30"
        );
        let (cells, rule) = cells(&p, &w, 1, Some(Parting::Gap), Rect::new(0, 0, 30, 3), 0, 1);
        assert!(rule.is_none(), "a gap, not a rule, parts them");
        let sess = cells.iter().find(|c| c.idx == 0).unwrap().rect;
        let host = cells.iter().find(|c| c.idx == 2).unwrap().rect;
        assert_eq!(sess.x, 0, "sessions at the left edge");
        assert_eq!(
            host.x + host.width,
            30,
            "the host card is pushed to the right edge"
        );
        assert!(host.x > sess.x + sess.width, "blank columns part the bands");
    }

    #[test]
    fn when_the_bands_would_touch_a_rule_parts_them() {
        // Sessions (10) + host (10) in a 20-wide band: a rule column would not fit, so
        // the parting is a RULE that takes the boundary's column, and the run scrolls.
        let mut cards = run(2, 10);
        let boundary = cards.len();
        cards.extend(host(10, 1));
        let p = place(&cards, 3, boundary);
        let w = widths(&cards, &p, 100);
        assert_eq!(
            parting(&w, 1, 20, 1),
            Some(Parting::Rule),
            "sess 10 + host 10 + rule 1 exceeds 20"
        );
        let dw = display_widths(&w, 1, Parting::Rule);
        assert_eq!(dw, vec![10, 1, 10], "the rule takes the boundary's column");
        let (cells, rule) = cells(&p, &w, 1, Some(Parting::Rule), Rect::new(0, 0, 20, 3), 0, 1);
        let rule = rule.expect("a rule parts the touching bands");
        assert_eq!(
            rule.x, 11,
            "the rule stands past the session column and its gutter"
        );
        let sess = cells.iter().find(|c| c.idx == 0).unwrap().rect;
        assert_eq!(sess.x, 0, "the session column holds the left edge");
        assert!(
            cells.iter().find(|c| c.idx == 2).is_none(),
            "the host band is off screen until the run scrolls"
        );
    }

    #[test]
    fn one_band_alone_parts_nothing() {
        // Only sessions: no parting, and the flow is plain left-to-right.
        let cards = run(3, 10);
        let p = place(&cards, 2, cards.len());
        let w = widths(&cards, &p, 100);
        assert_eq!(parting(&w, boundary_col(&p, cards.len()), 30, 1), None);
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
        let mut all = run(2, 10);
        all.extend(run(2, 10));
        all.extend(run(2, 10));
        let p = place(&all, 3, all.len()); // one section per column
        assert_eq!(
            hidden_counts(&p, 0, None, 0, 1),
            (0, 4),
            "two columns hide to the right"
        );
        assert_eq!(hidden_counts(&p, 0, None, 1, 1), (2, 2), "one either side");
        assert_eq!(
            hidden_counts(&p, 0, None, 2, 1),
            (4, 0),
            "all of them to the left"
        );
        assert_eq!(hidden_counts(&p, 0, None, 0, 3), (0, 0), "nothing hidden");
    }

    #[test]
    fn cells_place_columns_left_to_right_with_a_gutter() {
        let cards = run(2, 10);
        let p = place(&cards, 1, cards.len()); // one card per column
        let w = widths(&cards, &p, 100);
        let (first_cells, rule) = cells(&p, &w, 0, None, Rect::new(5, 3, 21, 2), 0, 1);
        assert!(rule.is_none());
        assert_eq!(first_cells.len(), 2);
        assert_eq!((first_cells[0].rect.x, first_cells[0].rect.y), (5, 3));
        assert_eq!(
            (first_cells[1].rect.x, first_cells[1].rect.y),
            (16, 3),
            "next column starts past the first plus the gutter"
        );
        // Scrolled one column right: the first column is neither painted nor clickable.
        let (scrolled, _) = cells(&p, &w, 0, None, Rect::new(5, 3, 21, 2), 1, 1);
        assert_eq!(scrolled.len(), 1);
        assert_eq!(scrolled[0].idx, 1);
        assert_eq!(
            scrolled[0].rect.x, 5,
            "the drawn column sits at the left edge"
        );
    }
}
