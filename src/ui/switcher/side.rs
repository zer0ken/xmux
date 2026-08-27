//! The side list's card geometry: where every card paints, where the rule parting the
//! two bands goes, and how far the list has scrolled. Pure over card HEIGHTS, so the
//! paint, the mouse hit-test and the tests all read one answer.

/// One card's placement inside the card region, in rows from its top edge. `h` is what
/// the card gets on screen, which is less than its own height when the region cuts it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Slot {
    pub idx: usize,
    pub y: u16,
    pub h: u16,
}

/// Where the side list's cards land this frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Flow {
    /// The cards with at least one row on screen, in list order.
    pub slots: Vec<Slot>,
    /// The row the parting rule paints on, when a rule is what parts the bands.
    pub rule_y: Option<u16>,
    /// The first card drawn: the scroll position, counted in cards.
    pub offset: usize,
    /// How many cards are drawn WHOLE, which is the scrollbar's viewport length.
    pub visible: usize,
    /// Whether the list is a scrolling run, which is what earns the scrollbar its strip.
    pub scrolls: bool,
}

/// The rows cards `from..to` take, counting the parting rule when the boundary falls
/// inside that run. The rule sits immediately above card `boundary`, so a run starting
/// exactly there still pays for it.
fn span(heights: &[u16], boundary: Option<usize>, from: usize, to: usize) -> u16 {
    let cards: u16 = heights[from..to].iter().sum();
    let rule = u16::from(matches!(boundary, Some(b) if from <= b && b < to));
    cards + rule
}

/// The scroll position that keeps card `selected` whole, moving the least it takes: down
/// far enough to reach it, then back up while the run to the list's end still fits, so the
/// last card never floats above the region's bottom edge with rows to spare.
fn scroll_to(
    heights: &[u16],
    boundary: Option<usize>,
    region_h: u16,
    offset: usize,
    selected: usize,
) -> usize {
    let n = heights.len();
    let mut off = offset.min(selected);
    while off < selected && span(heights, boundary, off, selected + 1) > region_h {
        off += 1;
    }
    while off > 0 && span(heights, boundary, off - 1, n) <= region_h {
        off -= 1;
    }
    off
}

/// Places the cards of a side list `region_h` rows tall, given each card's height and the
/// index of the first host-state card.
///
/// The list is two BANDS: the session cards, then the host-state cards (the hosts with no
/// session to show, which the flatten sinks to the end). While the cards can spare a row
/// for it, the bands are pushed APART - sessions against the top edge, host states against
/// the bottom - and the blank rows left between them are the parting, because a gap says "a
/// different kind of thing follows" without spending a glyph on saying it. Once they cannot
/// the list is one scrolling run, since a gap only parts what is on screen together, and a
/// rule takes the boundary's row instead.
///
/// The parting therefore always has a row of its own: the run is measured with the rule's
/// row included, so the bands go from a gap of one straight to a rule and never touch. That
/// makes the list scroll one row earlier than the cards alone would need.
///
/// A boundary with a band empty on either side parts nothing, so it is dropped here: this
/// is the one place that rule lives.
pub(super) fn place(
    heights: &[u16],
    boundary: Option<usize>,
    region_h: u16,
    offset: usize,
    selected: usize,
) -> Flow {
    let n = heights.len();
    if n == 0 || region_h == 0 {
        return Flow::default();
    }
    // A boundary of 0 is the list with NOTHING but host-state cards - the empty
    // session band - which the filter below would drop as "no parting". The band still
    // anchors to the bottom edge then, so the change is kept here.
    let host_only = boundary == Some(0);
    let boundary = boundary.filter(|&b| b > 0 && b < n);
    let total: u16 = heights.iter().sum();
    // The parting's own row is part of what the region has to hold, so the bands never
    // meet with nothing between them.
    if total + u16::from(boundary.is_some()) <= region_h {
        let gap = region_h - total;
        let mut slots = Vec::with_capacity(n);
        // A boundary of 0 was dropped above, meaning the whole list is host-state cards
        // (nothing has a session to show yet): their band is the ONLY band, so it
        // anchors to the BOTTOM edge and the blank rows above it are where the sessions
        // that will be found land. With real bands, the session band holds the top edge.
        let mut y = if host_only { gap } else { 0 };
        for (i, &h) in heights.iter().enumerate() {
            if Some(i) == boundary {
                y += gap;
            }
            slots.push(Slot { idx: i, y, h });
            y += h;
        }
        return Flow {
            slots,
            rule_y: None,
            offset: 0,
            visible: n,
            scrolls: false,
        };
    }
    let offset = scroll_to(heights, boundary, region_h, offset, selected.min(n - 1));
    let mut slots = Vec::new();
    let mut rule_y = None;
    let mut visible = 0usize;
    let mut y = 0u16;
    for (i, &card_h) in heights.iter().enumerate().skip(offset) {
        if Some(i) == boundary {
            if y >= region_h {
                break;
            }
            rule_y = Some(y);
            y += 1;
        }
        if y >= region_h {
            break;
        }
        let h = card_h.min(region_h - y);
        slots.push(Slot { idx: i, y, h });
        if h == card_h {
            visible += 1;
        }
        y += h;
    }
    Flow {
        slots,
        rule_y,
        offset,
        visible,
        scrolls: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ys(flow: &Flow) -> Vec<(usize, u16, u16)> {
        flow.slots.iter().map(|s| (s.idx, s.y, s.h)).collect()
    }

    #[test]
    fn one_band_stacks_from_the_top() {
        let flow = place(&[2, 2, 2], None, 20, 0, 0);
        assert_eq!(ys(&flow), vec![(0, 0, 2), (1, 2, 2), (2, 4, 2)]);
        assert_eq!(flow.rule_y, None);
    }

    #[test]
    fn the_bands_part_with_the_rows_left_over() {
        // 3 session cards, 2 host-state cards, 20 rows: 10 rows of cards, 10 of gap.
        let flow = place(&[2, 2, 2, 2, 2], Some(3), 20, 0, 0);
        assert_eq!(
            ys(&flow),
            vec![(0, 0, 2), (1, 2, 2), (2, 4, 2), (3, 16, 2), (4, 18, 2)]
        );
        assert_eq!(flow.rule_y, None, "a gap parts them, not a rule");
        assert_eq!(flow.offset, 0);
    }

    #[test]
    fn the_bands_never_touch() {
        // The cards alone fill the region exactly, so the parting has no row left to take:
        // the list turns into a scrolling run one row before the cards overflow it.
        let flow = place(&[2, 2], Some(1), 4, 0, 0);
        assert!(flow.scrolls);
        assert_eq!(flow.rule_y, Some(2));
        // One row to spare is a gap, and a gap needs no scrolling.
        let fits = place(&[2, 2], Some(1), 5, 0, 0);
        assert!(!fits.scrolls);
        assert_eq!(ys(&fits), vec![(0, 0, 2), (1, 3, 2)]);
    }

    #[test]
    fn one_band_keeps_every_row_for_its_cards() {
        // No boundary, nothing to part: cards filling the region exactly still fit.
        let flow = place(&[2, 2], None, 4, 0, 0);
        assert!(!flow.scrolls);
        assert_eq!(ys(&flow), vec![(0, 0, 2), (1, 2, 2)]);
    }

    #[test]
    fn a_boundary_with_an_empty_band_parts_nothing() {
        // A list with NOTHING but host cards is the host band alone: it parts nothing
        // (no rule, no second band) but anchors to the BOTTOM, so the blank rows above
        // it are where the sessions that will be found land.
        let all_hosts = place(&[2, 2], Some(0), 20, 0, 0);
        assert_eq!(ys(&all_hosts), vec![(0, 16, 2), (1, 18, 2)]);
        // A list of sessions alone fills from the top, the host band being absent.
        let no_hosts = place(&[2, 2], Some(2), 20, 0, 0);
        assert_eq!(ys(&no_hosts), vec![(0, 0, 2), (1, 2, 2)]);
    }

    #[test]
    fn overflow_closes_the_gap_and_draws_the_rule() {
        // 5 cards of 2 rows plus the rule is 11 rows in 6: one scrolling run.
        let flow = place(&[2, 2, 2, 2, 2], Some(1), 6, 0, 0);
        assert_eq!(flow.rule_y, Some(2), "the rule takes the boundary's row");
        assert_eq!(ys(&flow), vec![(0, 0, 2), (1, 3, 2), (2, 5, 1)]);
        assert_eq!(flow.visible, 2, "the cut-off card is not a viewport card");
    }

    #[test]
    fn a_boundary_below_the_fold_draws_no_rule() {
        let flow = place(&[2, 2, 2, 2, 2], Some(3), 6, 0, 0);
        assert_eq!(flow.rule_y, None);
        assert_eq!(ys(&flow), vec![(0, 0, 2), (1, 2, 2), (2, 4, 2)]);
    }

    #[test]
    fn scrolling_keeps_the_selected_card_whole() {
        let heights = [2, 2, 2, 2, 2];
        let flow = place(&heights, Some(3), 6, 0, 4);
        assert_eq!(flow.offset, 3);
        assert_eq!(
            ys(&flow),
            vec![(3, 1, 2), (4, 3, 2)],
            "the rule leads the band it opens"
        );
        assert_eq!(flow.rule_y, Some(0));
    }

    #[test]
    fn the_rule_scrolls_away_with_its_boundary() {
        let flow = place(&[2, 2, 2, 2, 2, 2, 2], Some(1), 6, 4, 6);
        assert_eq!(flow.rule_y, None);
        assert_eq!(flow.slots.first().map(|s| s.idx), Some(4));
    }

    #[test]
    fn the_list_never_floats_above_its_bottom_edge() {
        // An offset left behind by a taller region is pulled back so the run ends flush.
        let flow = place(&[2, 2, 2, 2], Some(3), 6, 3, 3);
        assert_eq!(flow.offset, 2);
        assert_eq!(ys(&flow), vec![(2, 0, 2), (3, 3, 2)]);
    }

    #[test]
    fn an_empty_list_places_nothing() {
        assert_eq!(place(&[], None, 20, 0, 0), Flow::default());
        assert_eq!(place(&[2], None, 0, 0, 0), Flow::default());
    }
}
