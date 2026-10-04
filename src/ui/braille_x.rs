//! Fixed, monochrome Braille frames sampled from the final X rotation prototype.
//! The atlas keeps browser font rasterization and emoji fallback out of the TUI loop.

use ratatui::{layout::Rect, Frame};

const GLYPHS: usize = 14;
const FRAMES_PER_GLYPH: usize = 14;
const HOLD_MS: u64 = 1_000;
const TURN_MS: u64 = 400;
const BEAT_MS: u64 = 33;
const PERIOD_MS: u64 = HOLD_MS + TURN_MS;
const WIDTH: usize = 32;
const HEIGHT: usize = 16;
const FRAMES: &[u8] = include_bytes!("braille_x/frames_32.bin");

pub(crate) fn fits(area: Rect) -> bool {
    area.width >= WIDTH as u16 && area.height >= HEIGHT as u16
}

fn frame_index(elapsed_ms: u64) -> usize {
    let glyph = (elapsed_ms / PERIOD_MS % GLYPHS as u64) as usize;
    let phase = elapsed_ms % PERIOD_MS;
    let turn_frame = if phase < HOLD_MS {
        0
    } else {
        1 + ((phase - HOLD_MS) / BEAT_MS).min(12) as usize
    };
    glyph * FRAMES_PER_GLYPH + turn_frame
}

/// Paints the selected frame, centered and clipped to the terminal-view region.
/// Only Braille cells are written; neither RGB nor a terminal-dependent shade is used.
pub(crate) fn render(frame: &mut Frame, area: Rect, elapsed_ms: u64) {
    if area.is_empty() {
        return;
    }
    let offset = frame_index(elapsed_ms) * WIDTH * HEIGHT;
    let cells = &FRAMES[offset..offset + WIDTH * HEIGHT];
    let left = area.x as i32 + (area.width as i32 - WIDTH as i32) / 2;
    let top = area.y as i32 + (area.height as i32 - HEIGHT as i32) / 2;
    let buffer = frame.buffer_mut();
    for (index, dot) in cells.iter().enumerate() {
        let x = left + (index % WIDTH) as i32;
        let y = top + (index / WIDTH) as i32;
        if x < area.x as i32
            || x >= area.right() as i32
            || y < area.y as i32
            || y >= area.bottom() as i32
        {
            continue;
        }
        let mut utf8 = [0; 4];
        let symbol = char::from_u32(0x2800 + u32::from(*dot))
            .unwrap()
            .encode_utf8(&mut utf8);
        buffer[(x as u16, y as u16)].set_symbol(symbol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_has_every_complete_braille_frame() {
        assert_eq!(FRAMES.len(), GLYPHS * FRAMES_PER_GLYPH * WIDTH * HEIGHT);
        assert!(FRAMES.iter().any(|dot| *dot != 0));
    }

    #[test]
    fn front_frame_fingerprints_preserve_the_approved_symbol_order() {
        // The names are the latest #439 sequence. Each checksum covers the complete
        // monochrome front frame after isolated-dot repair, not a font-dependent glyph.
        let symbols = [
            "X", "⚔️", "χ", "✂️", "Ж", "⚒️", "✘", "🛠️", "ж", "🦋", "✗", "🤞", "x", "☒",
        ];
        let expected = [
            16_539, 26_447, 22_047, 29_051, 24_898, 25_325, 16_976, 26_531, 18_159, 25_149, 17_871,
            23_370, 18_531, 58_169,
        ];
        for (index, symbol) in symbols.iter().enumerate() {
            let start = index * FRAMES_PER_GLYPH * WIDTH * HEIGHT;
            let sum: usize = FRAMES[start..start + WIDTH * HEIGHT]
                .iter()
                .map(|dot| usize::from(*dot))
                .sum();
            assert_eq!(sum, expected[index], "{symbol}");
        }
    }

    #[test]
    fn hold_turn_and_wrap_follow_the_prototype_clock() {
        assert_eq!(frame_index(0), frame_index(999));
        assert_ne!(frame_index(999), frame_index(1_000));
        assert_ne!(frame_index(1_000), frame_index(1_033));
        assert_eq!(frame_index(1_399), 13);
        assert_eq!(frame_index(1_400), 14);
        assert_eq!(frame_index(PERIOD_MS * GLYPHS as u64), 0);
    }

    #[test]
    fn settled_frames_do_not_have_isolated_missing_dots() {
        let (width, height, frames) = (WIDTH, HEIGHT, FRAMES);
        let bit = [[1, 8], [2, 16], [4, 32], [64, 128]];
        for frame in frames.chunks_exact(width * height) {
            let lit = |x: usize, y: usize| frame[(y / 4) * width + x / 2] & bit[y % 4][x % 2] != 0;
            assert!(frame.iter().any(|dot| *dot != 0));
            for y in 1..height * 4 - 1 {
                for x in 1..width * 2 - 1 {
                    assert!(
                        lit(x, y)
                            || !(lit(x - 1, y) && lit(x + 1, y) && lit(x, y - 1) && lit(x, y + 1)),
                        "isolated missing dot at {width}:{x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_same_frame_size_fits_large_and_small_areas() {
        assert!(fits(Rect::new(0, 0, 64, 33)));
        assert!(fits(Rect::new(0, 0, 32, 16)));
        assert!(!fits(Rect::new(0, 0, 31, 16)));
    }
}
