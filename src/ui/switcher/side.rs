//! The side list's card geometry: where every card paints, where the rule parting the
//! groups goes, and how far the list has scrolled. Pure over card HEIGHTS, so the
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
    /// How many cards are drawn whole.
    pub visible: usize,
    /// Whether the list is a scrolling run.
    pub scrolls: bool,
}

/// The rows cards `from..to` take, counting the parting rule when the boundary falls
/// inside that run. The rule sits immediately above card `boundary`, so a run starting
/// exactly there still pays for it.
fn span(heights: &[u16], boundaries: &[usize], from: usize, to: usize) -> u16 {
    let cards: u16 = heights[from..to].iter().sum();
    let rule = boundaries.iter().filter(|&&b| from <= b && b < to).count() as u16;
    cards + rule
}

/// The scroll position that keeps card `selected` whole, moving the least it takes: down
/// far enough to reach it, then back up while the run to the list's end still fits, so the
/// last card never floats above the region's bottom edge with rows to spare.
///
/// One card is spared the journey back down: when `selected` hangs in a section, whose
/// title is the row directly above its first card, and that title and the card fit on
/// screen together, the list shows the title instead of leaving it scrolled off the top
/// edge. When they do not fit, the card keeps what it needs and no more is scrolled than
/// it takes to show it.
fn scroll_to(
    heights: &[u16],
    boundaries: &[usize],
    region_h: u16,
    offset: usize,
    selected: usize,
    selected_section_title: Option<usize>,
) -> usize {
    let n = heights.len();
    let mut off = offset.min(selected);
    while off < selected && span(heights, boundaries, off, selected + 1) > region_h {
        off += 1;
    }
    while off > 0 && span(heights, boundaries, off - 1, n) <= region_h {
        off -= 1;
    }
    if let Some(t) = selected_section_title {
        if span(heights, boundaries, t, selected + 1) <= region_h {
            off = off.min(t);
        }
    }
    off
}

/// Places the cards in group order from the top, reserving one row at each group
/// boundary; rows below the last card stay empty.
/// Empty groups take no row. The run scrolls to keep the selected card visible.
/// The first scrolling group boundary can carry a horizontal rule; every
/// other group boundary remains blank.
///
/// `selected_section_title` is the index of the section title the selected card hangs
/// under, `None` when it hangs under none (a host-state card). When the card is close
/// enough to its own title to share the screen, the scroll keeps the title visible
/// instead of leaving it above the top edge; a card too far below its title keeps the
/// position that shows it.
pub(super) fn place(
    heights: &[u16],
    boundaries: impl IntoIterator<Item = usize>,
    region_h: u16,
    offset: usize,
    selected: usize,
    selected_section_title: Option<usize>,
) -> Flow {
    let n = heights.len();
    if n == 0 || region_h == 0 {
        return Flow::default();
    }
    let boundaries: Vec<_> = boundaries.into_iter().filter(|&b| b > 0 && b < n).collect();
    let total: u16 = heights.iter().sum();
    // The parting's own row is part of what the region has to hold, so the bands never
    // meet with nothing between them.
    if total + boundaries.len() as u16 <= region_h {
        let mut slots = Vec::with_capacity(n);
        let mut y = 0;
        for (i, &h) in heights.iter().enumerate() {
            if boundaries.contains(&i) {
                y += 1;
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
    let offset = scroll_to(
        heights,
        &boundaries,
        region_h,
        offset,
        selected.min(n - 1),
        selected_section_title,
    );
    let mut slots = Vec::new();
    let mut rule_y = None;
    let mut visible = 0usize;
    let mut y = 0u16;
    for (i, &card_h) in heights.iter().enumerate().skip(offset) {
        if boundaries.contains(&i) {
            if y >= region_h {
                break;
            }
            rule_y = rule_y.or(Some(y));
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
        let flow = place(&[2, 2, 2], None, 20, 0, 0, None);
        assert_eq!(ys(&flow), vec![(0, 0, 2), (1, 2, 2), (2, 4, 2)]);
        assert_eq!(flow.rule_y, None);
    }

    #[test]
    fn the_bands_part_with_the_rows_left_over() {
        // Both bands start at the top, with one blank row between them.
        let flow = place(&[2, 2, 2, 2, 2], Some(3), 20, 0, 0, None);
        assert_eq!(
            ys(&flow),
            vec![(0, 0, 2), (1, 2, 2), (2, 4, 2), (3, 7, 2), (4, 9, 2)]
        );
        assert_eq!(flow.rule_y, None, "a gap parts them, not a rule");
        assert_eq!(flow.offset, 0);
    }

    #[test]
    fn the_bands_never_touch() {
        // The cards alone fill the region exactly, so the parting has no row left to take:
        // the list turns into a scrolling run one row before the cards overflow it.
        let flow = place(&[2, 2], Some(1), 4, 0, 0, None);
        assert!(flow.scrolls);
        assert_eq!(flow.rule_y, Some(2));
        // One row to spare is a gap, and a gap needs no scrolling.
        let fits = place(&[2, 2], Some(1), 5, 0, 0, None);
        assert!(!fits.scrolls);
        assert_eq!(ys(&fits), vec![(0, 0, 2), (1, 3, 2)]);
    }

    #[test]
    fn one_band_keeps_every_row_for_its_cards() {
        // No boundary, nothing to part: cards filling the region exactly still fit.
        let flow = place(&[2, 2], None, 4, 0, 0, None);
        assert!(!flow.scrolls);
        assert_eq!(ys(&flow), vec![(0, 0, 2), (1, 2, 2)]);
    }

    #[test]
    fn a_boundary_with_an_empty_band_parts_nothing() {
        // A list with only host cards starts at the top like any other list.
        let all_hosts = place(&[2, 2], Some(0), 20, 0, 0, None);
        assert_eq!(ys(&all_hosts), vec![(0, 0, 2), (1, 2, 2)]);
        // A list of sessions alone fills from the top, the host band being absent.
        let no_hosts = place(&[2, 2], Some(2), 20, 0, 0, None);
        assert_eq!(ys(&no_hosts), vec![(0, 0, 2), (1, 2, 2)]);
    }

    #[test]
    fn overflow_closes_the_gap_and_draws_the_rule() {
        // 5 cards of 2 rows plus the rule is 11 rows in 6: one scrolling run.
        let flow = place(&[2, 2, 2, 2, 2], Some(1), 6, 0, 0, None);
        assert_eq!(flow.rule_y, Some(2), "the rule takes the boundary's row");
        assert_eq!(ys(&flow), vec![(0, 0, 2), (1, 3, 2), (2, 5, 1)]);
        assert_eq!(flow.visible, 2, "the cut-off card is not a viewport card");
    }

    #[test]
    fn a_boundary_below_the_fold_draws_no_rule() {
        let flow = place(&[2, 2, 2, 2, 2], Some(3), 6, 0, 0, None);
        assert_eq!(flow.rule_y, None);
        assert_eq!(ys(&flow), vec![(0, 0, 2), (1, 2, 2), (2, 4, 2)]);
    }

    #[test]
    fn scrolling_keeps_the_selected_card_whole() {
        let heights = [2, 2, 2, 2, 2];
        let flow = place(&heights, Some(3), 6, 0, 4, None);
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
        let flow = place(&[2, 2, 2, 2, 2, 2, 2], Some(1), 6, 4, 6, None);
        assert_eq!(flow.rule_y, None);
        assert_eq!(flow.slots.first().map(|s| s.idx), Some(4));
    }

    #[test]
    fn the_list_never_floats_above_its_bottom_edge() {
        // An offset left behind by a taller region is pulled back so the run ends flush.
        let flow = place(&[2, 2, 2, 2], Some(3), 6, 3, 3, None);
        assert_eq!(flow.offset, 2);
        assert_eq!(ys(&flow), vec![(2, 0, 2), (3, 3, 2)]);
    }

    #[test]
    fn a_first_section_card_brings_its_title_back() {
        // The selection hangs in the FIRST section and its title (row 0) can share the
        // screen with the card, so a scrolled-down offset is pulled back to show the
        // title instead of leaving it off the top edge.
        let flow = place(&[1, 1, 1, 1, 1, 1, 1, 1, 1], None, 5, 4, 4, Some(0));
        assert_eq!(flow.offset, 0, "the title comes back with the card");
        assert_eq!(flow.slots.first().map(|s| s.idx), Some(0));
        assert!(
            flow.slots.iter().any(|s| s.idx == 4),
            "the card stays whole"
        );
    }

    #[test]
    fn a_first_section_card_too_far_from_its_title_keeps_its_offset() {
        // The card is in the first section but the title sits too far above it to share
        // the screen: the list keeps the card visible and does not force the title in.
        let flow = place(&[1, 1, 1, 1, 1, 1, 1, 1, 1], None, 3, 4, 8, Some(0));
        assert_eq!(
            flow.offset, 6,
            "the card keeps what it needs, the title stays off"
        );
        assert!(
            flow.slots.iter().any(|s| s.idx == 8),
            "the card stays whole"
        );
    }

    #[test]
    fn a_middle_section_card_brings_its_own_title_back() {
        // A card in a later section pulls back to ITS OWN title, not the list's top: the
        // title directly above the card stays visible when it can share the screen. Rows
        // are three sections of title + two cards each; the wsl title sits at row 3.
        let flow = place(&[1, 1, 1, 1, 1, 1, 1, 1, 1], None, 5, 7, 5, Some(3));
        assert_eq!(
            flow.offset, 3,
            "the card's own title comes back, not the top row"
        );
        assert_eq!(flow.slots.first().map(|s| s.idx), Some(3));
        assert!(
            flow.slots.iter().any(|s| s.idx == 5),
            "the card stays whole"
        );
    }

    #[test]
    fn a_card_too_far_below_its_section_title_keeps_its_offset() {
        // The card's own title sits too far above it to share the screen: no pull, the
        // card keeps the position that shows it.
        let flow = place(&[1, 1, 1, 1, 1, 1, 1, 1, 1], None, 3, 7, 8, Some(4));
        assert_eq!(
            flow.offset, 6,
            "the card keeps what it needs, the title stays off"
        );
        assert!(
            flow.slots.iter().any(|s| s.idx == 8),
            "the card stays whole"
        );
    }

    #[test]
    fn an_empty_list_places_nothing() {
        assert_eq!(place(&[], None, 20, 0, 0, None), Flow::default());
        assert_eq!(place(&[2], None, 0, 0, 0, None), Flow::default());
    }
}
