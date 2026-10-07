//! The terminal behaviour a session's client expects that the vt100 cell model does
//! not keep: the answers to its terminal queries, the cursor shape it sets, and the
//! bells, notifications, and window title it sends, which the grid keeps for the loop.
//!
//! The vt100 parser hands every sequence it does not model to these callbacks, at its
//! place in the byte stream and whole however the reads split it, so each query the
//! client sent is answered exactly once. A client that waits for an answer the grid
//! never gives waits until its own timeout, and a client that reads the answer to
//! learn the terminal's features falls back to guessing from `TERM`.

use crate::display::outer::OuterTerminal;

/// The grid's primary device attributes: a VT220 (62) with ANSI colour (22). The grid
/// has no rectangular editing and no left and right margins, so it claims neither.
const PRIMARY_DA: &[u8] = b"\x1b[?62;22c";
/// The same with sixel graphics (4), claimed while the outer terminal can show the
/// sixel images the grid keeps.
const PRIMARY_DA_SIXEL: &[u8] = b"\x1b[?62;4;22c";

impl crate::display::image::layer::Replies for GridCallbacks {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }
}

#[derive(Default)]
pub struct GridCallbacks {
    replies: Vec<u8>,
    sink: crate::display::grid::ParserSink,
    /// The input modes the grid's mode scanner read, which outlive a wipe of the cells
    /// and so answer a mode report for them.
    input_modes: crate::display::modes::InputModes,
    /// The last DECSCUSR shape, `CSI Ps SP q`: 0 the terminal's default, 1 to 6 a
    /// blinking or steady block, underline, or bar.
    cursor_shape: u8,
}

impl GridCallbacks {
    pub fn set_input_modes(&mut self, modes: crate::display::modes::InputModes) {
        self.input_modes = modes;
    }

    /// The window title the client set, if it set one.
    pub fn title(&self) -> Option<&str> {
        self.sink.title.as_deref()
    }

    pub fn clear_title(&mut self) {
        self.sink.title = None;
    }

    /// The bells and notifications the client sent since the last call, oldest first.
    pub fn take_alerts(&mut self) -> Vec<crate::display::grid::Alert> {
        std::mem::take(&mut self.sink.alerts)
    }

    pub fn cursor_shape(&self) -> u8 {
        self.cursor_shape
    }

    /// The answers owed to the client since the last call, in the order it asked.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }

    fn reply_fmt(&mut self, args: std::fmt::Arguments) {
        use std::io::Write;
        let _ = self.replies.write_fmt(args);
    }

    /// The `DECRPM` value for a DEC private mode: 1 set, 2 reset, 0 a mode the grid
    /// does not keep.
    fn private_mode(&self, screen: &vt100::Screen, mode: u16) -> u8 {
        use vt100::{MouseProtocolEncoding as Enc, MouseProtocolMode as Mouse};
        let set = match mode {
            1 => screen.application_cursor(),
            25 => !screen.hide_cursor(),
            47 | 1049 => screen.alternate_screen(),
            9 => screen.mouse_protocol_mode() == Mouse::Press,
            1000 => screen.mouse_protocol_mode() == Mouse::PressRelease,
            1002 => screen.mouse_protocol_mode() == Mouse::ButtonMotion,
            1003 => screen.mouse_protocol_mode() == Mouse::AnyMotion,
            1005 => screen.mouse_protocol_encoding() == Enc::Utf8,
            1006 => screen.mouse_protocol_encoding() == Enc::Sgr,
            1004 => self.input_modes.focus_events,
            2004 => self.input_modes.bracketed_paste,
            _ => return 0,
        };
        if set {
            1
        } else {
            2
        }
    }

    fn device_status(&mut self, screen: &vt100::Screen, private: bool, op: u16) {
        let (row, col) = screen.cursor_position();
        match (private, op) {
            (false, 5) => self.reply(b"\x1b[0n"),
            (false, 6) => self.reply_fmt(format_args!("\x1b[{};{}R", row + 1, col + 1)),
            (true, 6) => self.reply_fmt(format_args!("\x1b[?{};{};1R", row + 1, col + 1)),
            (true, 996) => {
                if let Some(scheme) = crate::display::outer::outer().colour_scheme() {
                    self.reply_fmt(format_args!("\x1b[?997;{scheme}n"));
                }
            }
            _ => {}
        }
    }

    /// `CSI Ps t` window reports the grid can make: the text area in pixels (14), the
    /// cell in pixels (16), and the text area in cells (18). Pixels come from the
    /// terminal's own cell size, so without it the pixel reports stay unanswered.
    fn window_report(&mut self, screen: &vt100::Screen, op: u16) {
        let (rows, cols) = screen.size();
        let cell = crate::display::outer::outer().cell_px;
        match (op, cell) {
            (14, Some((h, w))) => self.reply_fmt(format_args!(
                "\x1b[4;{};{}t",
                u32::from(rows) * u32::from(h),
                u32::from(cols) * u32::from(w)
            )),
            (16, Some((h, w))) => self.reply_fmt(format_args!("\x1b[6;{h};{w}t")),
            (18, _) => self.reply_fmt(format_args!("\x1b[8;{rows};{cols}t")),
            _ => {}
        }
    }

    /// `OSC 10 ; ?`, `OSC 11 ; ?`, and `OSC 4 ; n ; ?` answered with the terminal's own
    /// colours; a colour the terminal never reported stays unanswered.
    fn colour_query(&mut self, params: &[&[u8]]) {
        let outer: OuterTerminal = crate::display::outer::outer();
        match params {
            [b"10", b"?"] => {
                if let Some(c) = &outer.foreground {
                    self.reply_fmt(format_args!("\x1b]10;{c}\x1b\\"));
                }
            }
            [b"11", b"?"] => {
                if let Some(c) = &outer.background {
                    self.reply_fmt(format_args!("\x1b]11;{c}\x1b\\"));
                }
            }
            [b"4", pairs @ ..] => {
                for pair in pairs.chunks(2) {
                    let [index, b"?"] = pair else {
                        continue;
                    };
                    let Some(n) = std::str::from_utf8(index)
                        .ok()
                        .and_then(|s| s.parse::<usize>().ok())
                    else {
                        continue;
                    };
                    if let Some(Some(c)) = outer.palette.get(n) {
                        self.reply_fmt(format_args!("\x1b]4;{n};{c}\x1b\\"));
                    }
                }
            }
            _ => {}
        }
    }
}

/// The first value of the `n`th parameter, or 0 when it is absent.
fn param(params: &[&[u16]], n: usize) -> u16 {
    params.get(n).and_then(|p| p.first()).copied().unwrap_or(0)
}

impl vt100::Callbacks for GridCallbacks {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        match (i1, i2, c) {
            // Primary device attributes.
            (None, None, 'c') if param(params, 0) == 0 => {
                self.reply(if crate::display::image::sixel_cell_px().is_some() {
                    PRIMARY_DA_SIXEL
                } else {
                    PRIMARY_DA
                })
            }
            // Secondary device attributes: no VT model number (0), the xmux version, and
            // no ROM cartridge (0).
            (Some(b'>'), None, 'c') if param(params, 0) == 0 => {
                self.reply_fmt(format_args!("\x1b[>0;{};0c", version_number()))
            }
            // XTVERSION: the terminal's name and version.
            (Some(b'>'), None, 'q') if param(params, 0) == 0 => self.reply_fmt(format_args!(
                "\x1bP>|xmux({})\x1b\\",
                env!("CARGO_PKG_VERSION")
            )),
            (None, None, 'n') => self.device_status(screen, false, param(params, 0)),
            (Some(b'?'), None, 'n') => self.device_status(screen, true, param(params, 0)),
            (None, None, 't') => self.window_report(screen, param(params, 0)),
            // DECRQM for a DEC private mode, then for an ANSI mode. The grid keeps no
            // ANSI mode, so each answers "not recognized".
            (Some(b'?'), Some(b'$'), 'p') => {
                let mode = param(params, 0);
                let value = self.private_mode(screen, mode);
                self.reply_fmt(format_args!("\x1b[?{mode};{value}$y"));
            }
            (Some(b' '), None, 'q') => {
                if let Ok(shape @ 0..=6) = u8::try_from(param(params, 0)) {
                    self.cursor_shape = shape;
                }
            }
            // DECSTR, the soft reset, returns the cursor to the default shape.
            (Some(b'!'), None, 'p') => self.cursor_shape = 0,
            (Some(b'$'), None, 'p') => {
                self.reply_fmt(format_args!("\x1b[{};0$y", param(params, 0)))
            }
            // The kitty keyboard protocol's flags, answered only while xmux's own
            // terminal has the protocol: a client told it is there expects its keys
            // encoded that way, which only that terminal can do.
            (Some(b'?'), None, 'u') if crate::display::keyboard::supported() => {
                let flags = self.input_modes.keyboard_flags;
                self.reply_fmt(format_args!("\x1b[?{flags}u"))
            }
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        self.colour_query(params);
        vt100::Callbacks::unhandled_osc(&mut self.sink, screen, params);
    }

    fn audible_bell(&mut self, screen: &mut vt100::Screen) {
        vt100::Callbacks::audible_bell(&mut self.sink, screen);
    }

    fn set_window_title(&mut self, screen: &mut vt100::Screen, title: &[u8]) {
        vt100::Callbacks::set_window_title(&mut self.sink, screen, title);
    }
}

/// The xmux version as one number, `major * 10000 + minor * 100 + patch`.
pub(crate) fn version_number() -> u32 {
    let mut parts = env!("CARGO_PKG_VERSION")
        .split(['.', '-'])
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let mut next = || parts.next().unwrap_or(0);
    next() * 10000 + next() * 100 + next()
}
