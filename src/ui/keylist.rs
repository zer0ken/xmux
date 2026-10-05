//! The prefix key list: the box a live prefix opens from the prefix indicator toward the
//! terminal view, naming every key the prefix unlocks, grouped by section and read from
//! the one key table. It lays its keys out in as many columns as the room beside the
//! indicator holds. When they do not fit it first shortens every description, then gives
//! up the least needed keys and says how many with `+N more`, so a key is never shown
//! without its name.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::model::keys::{Section, TABLE};
use crate::model::NavPosition;
use crate::ui::palette;

/// Blank cells between two key columns.
const GAP: u16 = 2;
/// Blank cells between the border and the keys on each side.
const PAD: u16 = 1;

/// One cell of a key column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cell {
    /// A section title.
    Title(String),
    /// A key and its description.
    Key { key: String, desc: String },
    /// How many keys were given up.
    More(usize),
}

impl Cell {
    fn width(&self, key_width: u16) -> u16 {
        match self {
            Cell::Title(t) => t.width() as u16,
            Cell::Key { desc, .. } => key_width + 1 + desc.width() as u16,
            Cell::More(n) => more_text(*n).width() as u16,
        }
    }
}

fn more_text(n: usize) -> String {
    format!("+{n} more")
}

/// How much the list gave up to fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rung {
    /// Every key with its full description.
    Long,
    /// Every key with its short description.
    Short,
    /// Short descriptions, and some keys given up behind `+N more`.
    Dropped,
}

/// The laid-out list: its columns top to bottom, left to right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyList {
    pub(crate) columns: Vec<Vec<Cell>>,
    pub(crate) key_width: u16,
    pub(crate) column_width: u16,
    pub(crate) rung: Rung,
}

impl KeyList {
    /// The box's outer size, borders included.
    pub(crate) fn size(&self) -> (u16, u16) {
        let n = self.columns.len() as u16;
        let w = n * self.column_width + n.saturating_sub(1) * GAP + 2 * PAD + 2;
        let h = self.columns.iter().map(Vec::len).max().unwrap_or(0) as u16 + 2;
        (w, h)
    }

    /// Every key the list shows.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<&str> {
        self.columns
            .iter()
            .flatten()
            .filter_map(|c| match c {
                Cell::Key { key, .. } => Some(key.as_str()),
                _ => None,
            })
            .collect()
    }

    /// How many keys were given up, 0 when every key is shown.
    #[cfg(test)]
    pub(crate) fn more(&self) -> usize {
        self.columns
            .iter()
            .flatten()
            .find_map(|c| match c {
                Cell::More(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0)
    }
}

struct Item {
    section: Section,
    key: String,
    long: &'static str,
    short: &'static str,
    rank: u8,
}

fn items(prefix: &str, position: NavPosition) -> Vec<Item> {
    TABLE
        .iter()
        .filter(|e| e.prefixed())
        .map(|e| Item {
            section: e.section,
            key: e.key_label(prefix, position),
            long: e.long,
            short: e.short,
            rank: e.rank,
        })
        .collect()
}

/// The key list for a box of at most `max_w` by `max_h` cells, borders included, or
/// `None` when not even the keys that are never given up fit.
pub(crate) fn key_list(
    prefix: &str,
    position: NavPosition,
    max_w: u16,
    max_h: u16,
) -> Option<KeyList> {
    let items = items(prefix, position);
    let all: Vec<&Item> = items.iter().collect();
    for rung in [Rung::Long, Rung::Short] {
        if let Some(list) = pack(&all, rung, 0, max_w, max_h) {
            return Some(list);
        }
    }
    // The least needed key goes first: the highest rank, and among equals the one the
    // table lists last.
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| items[i].rank > 0).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(items[i].rank), std::cmp::Reverse(i)));
    let mut dropped: Vec<usize> = Vec::new();
    for i in order {
        dropped.push(i);
        let kept: Vec<&Item> = (0..items.len())
            .filter(|j| !dropped.contains(j))
            .map(|j| &items[j])
            .collect();
        if let Some(list) = pack(&kept, Rung::Dropped, dropped.len(), max_w, max_h) {
            return Some(list);
        }
    }
    None
}

fn pack(items: &[&Item], rung: Rung, more: usize, max_w: u16, max_h: u16) -> Option<KeyList> {
    if max_w <= 2 + 2 * PAD || max_h < 4 {
        return None;
    }
    let inner_w = max_w - 2 - 2 * PAD;
    let inner_h = max_h - 2;
    let mut blocks: Vec<Vec<Cell>> = Vec::new();
    for section in Section::ALL {
        let keys: Vec<Cell> = items
            .iter()
            .filter(|i| i.section == section)
            .map(|i| Cell::Key {
                key: i.key.clone(),
                desc: if rung == Rung::Long { i.long } else { i.short }.to_string(),
            })
            .collect();
        if !keys.is_empty() {
            let mut block = vec![Cell::Title(section.title().to_string())];
            block.extend(keys);
            blocks.push(block);
        }
    }
    if more > 0 {
        blocks.push(vec![Cell::More(more)]);
    }
    let key_width = items.iter().map(|i| i.key.width()).max().unwrap_or(0) as u16;
    let column_width = blocks
        .iter()
        .flatten()
        .map(|c| c.width(key_width))
        .max()
        .unwrap_or(0);
    if column_width > inner_w {
        return None;
    }
    let max_cols = ((inner_w + GAP) / (column_width + GAP)) as usize;
    // The shortest box that fits the room's width: the keys spread across every column
    // the room holds before the box grows a row.
    (2..=inner_h).find_map(|h| {
        let columns = flow(&blocks, h as usize);
        (columns.len() <= max_cols).then_some(KeyList {
            columns,
            key_width,
            column_width,
            rung,
        })
    })
}

/// Runs the blocks down columns `h` cells tall. A section starts a new column when it
/// would not fit in what is left of the current one but fits a column of its own, and a
/// title never ends a column, so a title always stands over a key of its section.
fn flow(blocks: &[Vec<Cell>], h: usize) -> Vec<Vec<Cell>> {
    let mut columns: Vec<Vec<Cell>> = vec![Vec::new()];
    for block in blocks {
        let len = columns.last().map_or(0, Vec::len);
        if len > 0 && len + block.len() > h && block.len() <= h {
            columns.push(Vec::new());
        }
        for (n, cell) in block.iter().enumerate() {
            let len = columns.last().map_or(0, Vec::len);
            let orphan = matches!(cell, Cell::Title(_)) && n + 1 < block.len() && len + 1 == h;
            if len == h || (orphan && len > 0) {
                columns.push(Vec::new());
            }
            if let Some(column) = columns.last_mut() {
                column.push(cell.clone());
            }
        }
    }
    columns.retain(|c| !c.is_empty());
    columns
}

/// The room the list may take: from the prefix indicator toward the terminal view. Beside
/// a side column it is the terminal view's columns down to the indicator's row (the whole
/// window's width when the terminal view is too narrow for a box), under a top band's
/// seam the rows below it, over a bottom band's seam the rows above it, and the whole
/// window when the nav is hidden and there is no indicator.
pub(crate) fn room(indicator: Rect, terminal: Rect, area: Rect, position: NavPosition) -> Rect {
    if indicator.height == 0 {
        return area;
    }
    match position {
        NavPosition::Left | NavPosition::Right => {
            let h = indicator.bottom().saturating_sub(area.y);
            if terminal.width >= MIN_SIDE_ROOM {
                Rect {
                    x: terminal.x,
                    y: area.y,
                    width: terminal.width,
                    height: h,
                }
            } else {
                Rect { height: h, ..area }
            }
        }
        NavPosition::Top => Rect {
            x: area.x,
            y: indicator.bottom(),
            width: area.width,
            height: area.bottom().saturating_sub(indicator.bottom()),
        },
        NavPosition::Bottom => Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: indicator.y.saturating_sub(area.y),
        },
    }
}

/// The narrowest terminal view a side column's key list opens into before it takes the
/// whole window's width instead.
const MIN_SIDE_ROOM: u16 = 24;

/// Where a box of `size` sits in `room`: against the indicator. Beside a left column it
/// keeps to the room's bottom left, beside a right column its bottom right, under a top
/// band its top right, over a bottom band its bottom right, and with the nav hidden the
/// window's bottom left.
pub(crate) fn place(room: Rect, position: NavPosition, nav_hidden: bool, size: (u16, u16)) -> Rect {
    let w = size.0.min(room.width);
    let h = size.1.min(room.height);
    let left = room.x;
    let right = room.right().saturating_sub(w);
    let top = room.y;
    let bottom = room.bottom().saturating_sub(h);
    let (x, y) = if nav_hidden {
        (left, bottom)
    } else {
        match position {
            NavPosition::Left => (left, bottom),
            NavPosition::Right | NavPosition::Bottom => (right, bottom),
            NavPosition::Top => (right, top),
        }
    };
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// A key cell's style: bold in the accent. Every surface that lists keys in rows (the key
/// list, the command palette, the help) paints its key column with it.
pub(crate) fn key_cell_style(palette: &palette::Palette) -> Style {
    palette::interaction_key_style().fg(palette.accent)
}

/// A section title's style in a key listing: muted.
pub(crate) fn title_style(palette: &palette::Palette) -> Style {
    Style::default().fg(palette.disabled)
}

/// What the key list's borders say: the prefix it is titled with, the hidden
/// host count on the bottom border's left, and the xmux version on its right.
pub(crate) struct Border<'a> {
    pub(crate) prefix: &'a str,
    pub(crate) status: &'a str,
    pub(crate) version: &'a str,
}

/// Paints the list in `rect`: a rounded box titled with the prefix, the nav status and the
/// xmux version on its bottom border where they fit, and the columns inside. The status
/// is said first: the version takes the border only where both leave a corner's worth of
/// rule on each side.
pub(crate) fn render(
    frame: &mut Frame,
    rect: Rect,
    list: &KeyList,
    border: Border<'_>,
    palette: &palette::Palette,
) {
    let Border {
        prefix,
        status,
        version,
    } = border;
    frame.render_widget(Clear, rect);
    let mut block = crate::ui::modal::popup_block(prefix, "", rect.width, palette);
    let status_w = if status.is_empty() {
        0
    } else {
        status.width() as u16 + 2
    };
    let status_w = if status_w + 4 <= rect.width {
        status_w
    } else {
        0
    };
    if status_w > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {status} "),
                Style::default().fg(palette.decoration),
            ))
            .left_aligned(),
        );
    }
    // The version is a build pointer, so it takes the bottom border only where it leaves a
    // corner's worth of rule on each side.
    if status_w + (version.width() as u16) + 2 + 6 <= rect.width {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {version} "),
                Style::default().fg(palette.disabled),
            ))
            .right_aligned(),
        );
    }
    frame.render_widget(block, rect);
    let key_style = key_cell_style(palette);
    let title_style = title_style(palette);
    let x0 = rect.x + 1 + PAD;
    for (c, column) in list.columns.iter().enumerate() {
        let x = x0 + c as u16 * (list.column_width + GAP);
        for (r, cell) in column.iter().enumerate() {
            let y = rect.y + 1 + r as u16;
            if y + 1 >= rect.bottom() || x >= rect.right() {
                continue;
            }
            let line = match cell {
                Cell::Title(t) => Line::from(Span::styled(t.clone(), title_style)),
                Cell::Key { key, desc } => {
                    let pad = (list.key_width as usize).saturating_sub(key.width());
                    Line::from(vec![
                        Span::styled(key.clone(), key_style),
                        Span::raw(format!("{} {desc}", " ".repeat(pad))),
                    ])
                }
                Cell::More(n) => Line::from(Span::styled(more_text(*n), title_style)),
            };
            let cell_rect = Rect {
                x,
                y,
                width: list.column_width.min(rect.right().saturating_sub(x + 1)),
                height: 1,
            };
            frame.render_widget(Paragraph::new(line), cell_rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(list: &KeyList) -> Vec<String> {
        list.columns
            .iter()
            .flatten()
            .filter_map(|c| match c {
                Cell::Title(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    fn prefixed_count() -> usize {
        TABLE.iter().filter(|e| e.prefixed()).count()
    }

    #[test]
    fn a_wide_room_lists_every_key_grouped_in_columns_with_full_names() {
        let list = key_list("C-g", NavPosition::Left, 160, 30).unwrap();
        assert_eq!(list.rung, Rung::Long);
        assert_eq!(list.keys().len(), prefixed_count(), "every prefix key");
        assert!(list.columns.len() > 1, "more than one column: {list:?}");
        assert_eq!(titles(&list), ["navigate", "sessions", "view", "app"]);
        let (w, h) = list.size();
        assert!(w <= 160 && h <= 30);
        assert!(
            list.columns.iter().flatten().any(
                |c| matches!(c, Cell::Key { key, desc } if key == "/" && desc == "filter the list")
            ),
            "{list:?}"
        );
        assert!(
            list.columns.iter().flatten().any(
                |c| matches!(c, Cell::Key { key, desc } if key == "C-g" && desc == "send the prefix")
            ),
            "the literal prefix row writes the configured prefix"
        );
    }

    #[test]
    fn a_narrower_room_shortens_every_description_before_it_gives_up_a_key() {
        let long = key_list("C-g", NavPosition::Left, 160, 30).unwrap();
        let (long_w, _) = long.size();
        // Narrower than the long rung needs at the height the room leaves.
        let list = (40..=80)
            .filter_map(|width| key_list("C-g", NavPosition::Left, width, 14))
            .find(|list| list.rung == Rung::Short && list.more() == 0)
            .expect("a narrower room fits shortened keys without dropping them");
        assert!(long_w > list.size().0);
        assert_eq!(list.rung, Rung::Short, "{list:?}");
        assert_eq!(list.keys().len(), prefixed_count(), "no key given up");
        assert_eq!(list.more(), 0);
        assert!(list.columns.iter().flatten().all(|c| match c {
            Cell::Key { desc, .. } => TABLE.iter().any(|e| e.short == desc),
            _ => true,
        }));
    }

    #[test]
    fn a_small_room_gives_up_the_least_needed_keys_and_counts_them() {
        let list = key_list("C-g", NavPosition::Left, 30, 9).unwrap();
        assert_eq!(list.rung, Rung::Dropped, "{list:?}");
        let more = list.more();
        assert!(more > 0);
        assert_eq!(
            list.keys().len() + more,
            prefixed_count(),
            "kept and counted"
        );
        for key in ["1-9", "?", "q"] {
            assert!(
                list.keys().contains(&key),
                "{key} is never given up: {list:?}"
            );
        }
        let (w, h) = list.size();
        assert!(w <= 30 && h <= 9, "{w}x{h}");
        // Every key shown keeps a name: no rung leaves a bare key.
        assert!(list.columns.iter().flatten().all(|c| match c {
            Cell::Key { desc, .. } => !desc.is_empty(),
            _ => true,
        }));
    }

    #[test]
    fn a_title_never_ends_a_column() {
        for (w, h) in [(160, 30), (100, 8), (60, 12), (40, 10), (30, 9), (26, 6)] {
            if let Some(list) = key_list("C-g", NavPosition::Left, w, h) {
                for column in &list.columns {
                    assert!(
                        !matches!(column.last(), Some(Cell::Title(_))),
                        "{w}x{h}: {column:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_room_too_small_for_the_kept_keys_shows_no_list() {
        assert!(key_list("C-g", NavPosition::Left, 10, 20).is_none());
        assert!(key_list("C-g", NavPosition::Left, 80, 3).is_none());
    }

    #[test]
    fn the_arrow_rows_name_the_pair_the_placement_makes_active() {
        let left = key_list("C-g", NavPosition::Left, 160, 30).unwrap();
        let right = key_list("C-g", NavPosition::Right, 160, 30).unwrap();
        let desc_of = |list: &KeyList, key: &str| {
            list.columns.iter().flatten().find_map(|c| match c {
                Cell::Key { key: k, desc } if k == key => Some(desc.clone()),
                _ => None,
            })
        };
        assert_eq!(desc_of(&left, "→/↓").as_deref(), Some("focus the terminal"));
        assert_eq!(
            desc_of(&right, "←/↑").as_deref(),
            Some("focus the terminal")
        );
    }
}
