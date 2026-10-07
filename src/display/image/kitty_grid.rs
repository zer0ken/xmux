//! The grid side of kitty graphics: the images a child transmitted, their placements,
//! and the answers the child is owed.
//!
//! A virtual placement (`U=1`) only records the image's size in cells: the child
//! draws the placeholder cells itself, as text. A direct placement writes marker cells
//! at the cursor, like a sixel image, under an id of its own, so the frame can show it
//! as placeholder cells sized for that placement.

use std::collections::HashMap;
use std::sync::Arc;

use super::kitty::{Command, KittyImage};
use super::layer::{self, Area, Replies};
use super::Caps;

/// The most images one grid keeps. Past it the oldest transmission is forgotten.
const MAX_IMAGES: usize = 64;

/// An image the frame can show: its data and the cells one placement of it covers.
#[derive(Clone, Debug)]
pub struct Placed {
    pub image: Arc<KittyImage>,
    pub cols: u16,
    pub rows: u16,
    /// The child's id for the image.
    client: u32,
}

#[derive(Default)]
pub struct KittyStore {
    /// The first chunk of a transmission still arriving, with the payload so far.
    pending: Option<Command>,
    /// The child's image ids, to the id xmux shows the image under.
    by_client: HashMap<u32, u32>,
    /// The child's image numbers (`I=`), to the id assigned for each.
    numbers: HashMap<u32, u32>,
    next_client: u32,
    /// Every image the frame can show, by xmux's id: each transmitted image, and each
    /// direct placement of one.
    placed: HashMap<u32, Placed>,
    /// Transmitted images, oldest first.
    order: Vec<u32>,
}

impl KittyStore {
    /// The image xmux shows under `id`.
    pub fn placed(&self, id: u32) -> Option<&Placed> {
        self.placed.get(&id)
    }

    /// The id xmux shows the child's image `client` under.
    pub fn by_client(&self, client: u32) -> Option<u32> {
        self.by_client.get(&client).copied()
    }

    /// Handles one graphics command (`body` follows the `G`).
    pub fn command<C: vt100::Callbacks + Replies>(
        &mut self,
        body: &[u8],
        caps: Caps,
        parser: &mut vt100::Parser<C>,
    ) {
        let mut cmd = Command::parse(body);
        if let Some(mut first) = self.pending.take() {
            // A later chunk carries only `m` (and perhaps `q`); the first chunk's keys
            // describe the whole transmission.
            first.payload.extend_from_slice(&cmd.payload);
            if cmd.num(b'm') == 1 {
                if first.payload.len() < layer::MAX_DATA {
                    self.pending = Some(first);
                }
                return;
            }
            cmd = first;
        } else if cmd.num(b'm') == 1 {
            self.pending = Some(cmd);
            return;
        }
        let reply = match cmd.char(b'a', 't') {
            'q' => Ok(None),
            't' => self.transmit(&mut cmd).map(|_| None),
            'T' => self
                .transmit(&mut cmd)
                .and_then(|client| self.place(&cmd, client, caps, parser)),
            'p' => self
                .client_of(&cmd)
                .ok_or("ENOENT:image not found")
                .and_then(|client| self.place(&cmd, client, caps, parser)),
            'd' => {
                self.delete(&cmd, parser);
                return;
            }
            _ => return,
        };
        self.answer(&cmd, reply, parser);
    }

    /// The child's id for the image a command names by `i` or `I`.
    fn client_of(&self, cmd: &Command) -> Option<u32> {
        match (cmd.num(b'i'), cmd.num(b'I')) {
            (0, 0) => None,
            (0, n) => self.numbers.get(&n).copied(),
            (i, _) => Some(i),
        }
    }

    /// Stores a transmitted image, returning the child's id for it.
    fn transmit(&mut self, cmd: &mut Command) -> Result<u32, &'static str> {
        if cmd.char(b't', 'd') != 'd' {
            // A file, temporary file or shared memory object names something on the
            // session's machine, which is not xmux's in general.
            return Err("EBADF:only direct transmission reaches this terminal");
        }
        let image = KittyImage::from_command(cmd).ok_or("EINVAL:image size unknown")?;
        let client = match (cmd.num(b'i'), cmd.num(b'I')) {
            (0, 0) => 0,
            (0, n) => {
                self.next_client = self.next_client.wrapping_add(1).max(1);
                let id = self.next_client | 0x8000_0000;
                self.numbers.insert(n, id);
                cmd.keys.insert(b'i', id.to_string());
                id
            }
            (i, _) => i,
        };
        let id = layer::next_id();
        if let Some(old) = self.by_client.insert(client, id) {
            self.placed.remove(&old);
            self.order.retain(|&o| o != old);
        }
        self.placed.insert(
            id,
            Placed {
                image: Arc::new(image),
                cols: 0,
                rows: 0,
                client,
            },
        );
        self.order.push(id);
        if self.order.len() > MAX_IMAGES {
            let oldest = self.order.remove(0);
            if let Some(p) = self.placed.remove(&oldest) {
                self.by_client.remove(&p.client);
            }
        }
        Ok(client)
    }

    /// Places the child's image `client`: a virtual placement records its size in
    /// cells; a direct placement writes marker cells at the cursor and moves the
    /// cursor past them unless `C=1`.
    fn place<C: vt100::Callbacks + Replies>(
        &mut self,
        cmd: &Command,
        client: u32,
        caps: Caps,
        parser: &mut vt100::Parser<C>,
    ) -> Result<Option<String>, &'static str> {
        let id = self.by_client(client).ok_or("ENOENT:image not found")?;
        let image = self.placed[&id].image.clone();
        let cells = |given: u32, px: u32, cell: Option<u16>| -> Option<u16> {
            match given {
                0 => cell.map(|c| px.div_ceil(u32::from(c.max(1))) as u16),
                n => Some(n.min(u32::from(u16::MAX)) as u16),
            }
        };
        let cols = cells(cmd.num(b'c'), image.width, caps.cell_px.map(|c| c.1))
            .ok_or("EINVAL:cell size unknown")?;
        let rows = cells(cmd.num(b'r'), image.height, caps.cell_px.map(|c| c.0))
            .ok_or("EINVAL:cell size unknown")?;
        let cols = cols.clamp(1, super::kitty::MAX_CELLS);
        let rows = rows.clamp(1, super::kitty::MAX_CELLS);
        if cmd.num(b'U') == 1 {
            if let Some(p) = self.placed.get_mut(&id) {
                p.cols = cols;
                p.rows = rows;
            }
            return Ok(None);
        }
        let screen = parser.screen();
        let (screen_rows, screen_cols) = screen.size();
        let (screen_rows, screen_cols) = (usize::from(screen_rows), usize::from(screen_cols));
        if screen_rows == 0 || screen_cols == 0 {
            return Ok(None);
        }
        let (cur_row, cur_col) = screen.cursor_position();
        let (cur_row, cur_col) = (
            usize::from(cur_row),
            usize::from(cur_col).min(screen_cols - 1),
        );
        let restore = screen.attributes_formatted();
        let rows_u = usize::from(rows).min(layer::MAX_CELLS);
        let cols_u = usize::from(cols)
            .min(layer::MAX_CELLS)
            .min(screen_cols - cur_col);
        let scroll = (cur_row + rows_u).saturating_sub(screen_rows).min(cur_row);
        let top = cur_row - scroll;
        // kitty leaves the cursor on the image's last row, one column past it.
        let cursor = if cmd.num(b'C') == 1 {
            (cur_row, cur_col)
        } else {
            (
                (top + rows_u - 1).min(screen_rows - 1),
                (cur_col + cols_u).min(screen_cols - 1),
            )
        };
        let alias = layer::next_id();
        self.placed.insert(
            alias,
            Placed {
                image,
                cols: cols_u as u16,
                rows: rows_u as u16,
                client,
            },
        );
        let out = layer::marker_bytes(
            alias,
            Area {
                scroll,
                top,
                left: cur_col,
                rows: rows_u,
                cols: cols_u,
                screen_rows,
            },
            cursor,
            &restore,
        );
        parser.process(&out);
        self.retain_shown(parser.screen());
        Ok(None)
    }

    /// Drops the direct placements no marker cell names any more.
    fn retain_shown(&mut self, screen: &vt100::Screen) {
        let shown = layer::marker_ids(screen);
        let by_client = &self.by_client;
        self.placed
            .retain(|id, p| shown.contains(id) || by_client.get(&p.client) == Some(id));
    }

    /// `a=d`: removes all placements (`d=a`) or one image's (`d=i`), and with the
    /// uppercase form the image data too. A direct placement's marker cells are
    /// erased; a virtual placement stops showing its placeholder cells.
    fn delete<C: vt100::Callbacks>(&mut self, cmd: &Command, parser: &mut vt100::Parser<C>) {
        let what = cmd.char(b'd', 'a');
        let client = match what.to_ascii_lowercase() {
            'a' => None,
            'i' => match self.client_of(cmd) {
                Some(c) => Some(c),
                None => return,
            },
            _ => return,
        };
        let hit = |p: &Placed| client.is_none_or(|c| p.client == c);
        let ids: std::collections::HashSet<u32> = self
            .placed
            .iter()
            .filter(|(_, p)| hit(p))
            .map(|(&id, _)| id)
            .collect();
        erase_markers(&ids, parser);
        let free = what.is_ascii_uppercase();
        let by_client = &self.by_client;
        self.placed
            .retain(|id, p| !ids.contains(id) || (!free && by_client.get(&p.client) == Some(id)));
        for p in self.placed.values_mut().filter(|p| hit(p)) {
            p.cols = 0;
            p.rows = 0;
        }
        if free {
            let placed = &self.placed;
            self.by_client.retain(|_, id| placed.contains_key(id));
            self.order.retain(|id| placed.contains_key(id));
        }
    }

    /// Answers a command the child did not quiet: `q=1` keeps only errors, `q=2` keeps
    /// nothing, and a command naming no image gets no answer.
    fn answer<C: vt100::Callbacks + Replies>(
        &self,
        cmd: &Command,
        result: Result<Option<String>, &'static str>,
        parser: &mut vt100::Parser<C>,
    ) {
        let quiet = cmd.num(b'q');
        let (i, n) = (cmd.num(b'i'), cmd.num(b'I'));
        if i == 0 && n == 0 {
            return;
        }
        let text = match result {
            Ok(_) if quiet >= 1 => return,
            Err(_) if quiet >= 2 => return,
            Ok(_) => "OK".to_string(),
            Err(e) => e.to_string(),
        };
        let mut keys = Vec::new();
        if i != 0 {
            keys.push(format!("i={i}"));
        }
        if n != 0 {
            keys.push(format!("I={n}"));
        }
        let reply = format!("\x1b_G{};{text}\x1b\\", keys.join(","));
        parser.callbacks_mut().reply(reply.as_bytes());
    }
}

/// Blanks every marker cell of the images `ids` names.
fn erase_markers<C: vt100::Callbacks>(
    ids: &std::collections::HashSet<u32>,
    parser: &mut vt100::Parser<C>,
) {
    if ids.is_empty() {
        return;
    }
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let (cur_row, cur_col) = screen.cursor_position();
    let mut out = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let hit = screen
                .cell(r, c)
                .and_then(layer::marker)
                .is_some_and(|p| ids.contains(&p.id));
            if hit {
                out.extend_from_slice(format!("\x1b[{};{}H\x1b[0m ", r + 1, c + 1).as_bytes());
            }
        }
    }
    if out.is_empty() {
        return;
    }
    let restore = screen.attributes_formatted();
    out.extend_from_slice(
        format!("\x1b[{};{}H", cur_row + 1, cur_col.min(cols - 1) + 1).as_bytes(),
    );
    out.extend_from_slice(&restore);
    parser.process(&out);
}

/// One image cell of a kitty image as the frame shows it: the id the outer terminal
/// knows the image under, and the cell's row and column in the placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KittyCell {
    pub id: u32,
    pub row: u16,
    pub col: u16,
}

impl KittyStore {
    /// How the frame shows `cell`: `None` when it is no kitty image cell, `Some(None)`
    /// for a placeholder cell naming nothing the grid can show, which stays blank as
    /// kitty leaves it. `left` is the cell to its left, from which a placeholder cell
    /// without a row or column diacritic takes them, as kitty infers them.
    pub fn cell(&self, cell: &vt100::Cell, left: Option<KittyCell>) -> Option<Option<KittyCell>> {
        if let Some(p) = layer::marker(cell) {
            return self.placed.contains_key(&p.id).then_some(Some(KittyCell {
                id: p.id,
                row: u16::from(p.row),
                col: u16::from(p.col),
            }));
        }
        let text = super::kitty::read_placeholder(cell.contents())?;
        let low = match cell.fgcolor() {
            vt100::Color::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
            vt100::Color::Idx(i) => u32::from(i),
            vt100::Color::Default => 0,
        };
        let client = low | u32::from(text.id_high.unwrap_or(0)) << 24;
        let Some(id) = self
            .by_client(client)
            .filter(|id| self.placed.get(id).is_some_and(|p| p.cols > 0))
        else {
            return Some(None);
        };
        let left = left.filter(|l| l.id == id);
        let row = text.row.or(left.map(|l| l.row)).unwrap_or(0);
        let col = text
            .col
            .or(left.filter(|l| l.row == row).map(|l| l.col + 1))
            .unwrap_or(0);
        Some(Some(KittyCell { id, row, col }))
    }
}
