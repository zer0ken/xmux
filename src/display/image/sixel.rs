//! Sixel decoding into an indexed-colour bitmap, and encoding of a rectangle of one.
//!
//! A sixel image reaches the outer terminal again only in pieces cut to the cells the
//! frame leaves to it, so the grid keeps the decoded pixels and encodes each piece on
//! demand. Colours stay palette indices: a piece re-uses the source's own registers,
//! so cropping never re-quantizes.

/// The largest image the grid keeps, in pixels per side. Pixels past it are dropped,
/// as a terminal clips an image at its own edge.
pub const MAX_SIDE: usize = 4096;

/// A decoded sixel image. `pixels` holds one palette index plus one per pixel, row
/// by row, and 0 where the image set nothing.
#[derive(Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u16>,
    /// RGB per colour register.
    pub palette: Vec<[u8; 3]>,
    /// The DCS `P2` parameter: 1 leaves unset pixels transparent, 0 and 2 fill them
    /// with the background.
    pub p2: u8,
}

/// The VT340 default colour registers, which xterm and Windows Terminal start from.
const DEFAULT_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],
    [51, 51, 204],
    [204, 36, 36],
    [51, 204, 51],
    [204, 51, 204],
    [51, 204, 204],
    [204, 204, 51],
    [120, 120, 120],
    [69, 69, 69],
    [87, 87, 153],
    [153, 69, 69],
    [87, 153, 87],
    [153, 87, 153],
    [87, 153, 153],
    [153, 153, 87],
    [204, 204, 204],
];

/// The number of colour registers a sixel string may define.
const REGISTERS: usize = 1024;

/// Decodes the body of a sixel DCS: `params` are the bytes between `ESC P` and the
/// final `q`, `data` the bytes after it up to the string terminator.
pub fn decode(params: &[u8], data: &[u8]) -> Bitmap {
    let p2 = params
        .split(|&b| b == b';')
        .nth(1)
        .and_then(|p| std::str::from_utf8(p).ok()?.parse::<u8>().ok())
        .unwrap_or(0);
    let mut palette: Vec<[u8; 3]> = DEFAULT_PALETTE.to_vec();
    palette.resize(REGISTERS, [0, 0, 0]);
    let mut d = Decoder {
        width: 0,
        height: 0,
        stride: 0,
        rows: 0,
        pixels: Vec::new(),
    };
    let (mut x, mut y) = (0usize, 0usize);
    let mut colour: u16 = 0;
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        match b {
            b'"' => {
                let (nums, next) = numbers(data, i + 1);
                i = next;
                if let [_, _, w, h, ..] = nums[..] {
                    d.width = d.width.max(w.min(MAX_SIDE));
                    d.height = d.height.max(h.min(MAX_SIDE));
                }
                continue;
            }
            b'#' => {
                let (nums, next) = numbers(data, i + 1);
                i = next;
                let Some(&reg) = nums.first() else { continue };
                let reg = reg.min(REGISTERS - 1);
                colour = reg as u16;
                if let [_, space, a, b2, c, ..] = nums[..] {
                    palette[reg] = match space {
                        1 => hls(a, b2, c),
                        _ => [pct(a), pct(b2), pct(c)],
                    };
                }
                continue;
            }
            b'!' => {
                let (nums, next) = numbers(data, i + 1);
                i = next;
                let count = nums.first().copied().unwrap_or(1).max(1);
                if let Some(&c) = data.get(i) {
                    if (0x3f..=0x7e).contains(&c) {
                        d.put(x, y, c - 0x3f, count, colour);
                        x += count;
                        i += 1;
                    }
                }
                continue;
            }
            b'$' => x = 0,
            b'-' => {
                x = 0;
                y += 6;
            }
            0x3f..=0x7e => {
                d.put(x, y, b - 0x3f, 1, colour);
                x += 1;
            }
            _ => {}
        }
        i += 1;
    }
    d.finish(palette, p2)
}

/// The bitmap under construction: `stride` x `rows` is the allocated area, `width` x
/// `height` the image's extent so far.
struct Decoder {
    width: usize,
    height: usize,
    stride: usize,
    rows: usize,
    pixels: Vec<u16>,
}

impl Decoder {
    /// Sets the pixels a sixel character `bits` draws at `(x, y)`, `count` times across.
    fn put(&mut self, x: usize, y: usize, bits: u8, count: usize, colour: u16) {
        if bits == 0 || x >= MAX_SIDE || y >= MAX_SIDE {
            // A blank sixel still widens the image, as it advances the drawing point.
            self.width = self.width.max((x + count).min(MAX_SIDE));
            return;
        }
        let end = (x + count).min(MAX_SIDE);
        let bottom = (y + 6).min(MAX_SIDE);
        self.grow(end, bottom);
        for (row, py) in (y..bottom).enumerate() {
            if bits & (1 << row) == 0 {
                continue;
            }
            let line = &mut self.pixels[py * self.stride..];
            line[x..end].fill(colour + 1);
            self.height = self.height.max(py + 1);
        }
        self.width = self.width.max(end);
    }

    fn grow(&mut self, w: usize, h: usize) {
        if w <= self.stride && h <= self.rows {
            return;
        }
        let stride = w.max(self.stride).next_power_of_two().min(MAX_SIDE);
        let rows = h.max(self.rows).next_power_of_two().min(MAX_SIDE);
        let mut pixels = vec![0u16; stride * rows];
        for r in 0..self.rows {
            pixels[r * stride..r * stride + self.stride]
                .copy_from_slice(&self.pixels[r * self.stride..(r + 1) * self.stride]);
        }
        self.pixels = pixels;
        self.stride = stride;
        self.rows = rows;
    }

    fn finish(self, palette: Vec<[u8; 3]>, p2: u8) -> Bitmap {
        let (w, h) = (self.width, self.height);
        let mut pixels = vec![0u16; w * h];
        for r in 0..h.min(self.rows) {
            let n = w.min(self.stride);
            pixels[r * w..r * w + n]
                .copy_from_slice(&self.pixels[r * self.stride..r * self.stride + n]);
        }
        Bitmap {
            width: w,
            height: h,
            pixels,
            palette,
            p2,
        }
    }
}

/// The `;`-separated decimal numbers starting at `i`, and the index after them.
fn numbers(data: &[u8], mut i: usize) -> (Vec<usize>, usize) {
    let mut out = Vec::new();
    let mut cur: Option<usize> = None;
    while let Some(&b) = data.get(i) {
        match b {
            b'0'..=b'9' => {
                cur = Some(
                    cur.unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(usize::from(b - b'0')),
                )
            }
            b';' => out.push(cur.take().unwrap_or(0)),
            _ => break,
        }
        i += 1;
    }
    if let Some(c) = cur {
        out.push(c);
    }
    (out, i)
}

/// A sixel colour percentage as an 8-bit channel.
fn pct(v: usize) -> u8 {
    (v.min(100) * 255 / 100) as u8
}

/// A sixel HLS colour (hue in degrees with blue at 0, lightness and saturation in
/// percent) as RGB.
fn hls(h: usize, l: usize, s: usize) -> [u8; 3] {
    let l = l.min(100) as f64 / 100.0;
    let s = s.min(100) as f64 / 100.0;
    // Sixel puts blue at 0 degrees; the usual HSL formula puts red there.
    let h = ((h % 360) as f64 + 240.0) % 360.0 / 360.0;
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

impl Bitmap {
    /// A complete sixel DCS for the pixels in `x..x+w`, `y..y+h`, clipped to the
    /// bitmap. It declares its own size and the source's `P2`, so the terminal fills
    /// or keeps the background as it would for the whole image.
    pub fn encode(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        let (w, h) = (x1.saturating_sub(x), y1.saturating_sub(y));
        let mut out = format!("\x1bP0;{};0q\"1;1;{w};{h}", self.p2).into_bytes();
        let mut used = vec![false; self.palette.len()];
        for py in y..y1 {
            for &p in &self.pixels[py * self.width + x..py * self.width + x1] {
                if p > 0 {
                    used[usize::from(p - 1)] = true;
                }
            }
        }
        for (reg, rgb) in self.palette.iter().enumerate() {
            if used[reg] {
                let [r, g, b] = rgb.map(|c| u32::from(c) * 100 / 255);
                out.extend_from_slice(format!("#{reg};2;{r};{g};{b}").as_bytes());
            }
        }
        let mut band = y;
        let mut row_bits = vec![0u8; w];
        while band < y1 {
            let mut first_colour = true;
            for (reg, _) in used.iter().enumerate().filter(|(_, u)| **u) {
                let colour = reg as u16 + 1;
                let mut any = false;
                for (col, bits) in row_bits.iter_mut().enumerate() {
                    *bits = 0;
                    for row in 0..6 {
                        let py = band + row;
                        if py < y1 && self.pixels[py * self.width + x + col] == colour {
                            *bits |= 1 << row;
                        }
                    }
                    any |= *bits != 0;
                }
                if !any {
                    continue;
                }
                if !first_colour {
                    out.push(b'$');
                }
                first_colour = false;
                out.extend_from_slice(format!("#{reg}").as_bytes());
                // Trailing blank columns need not be sent: the next colour starts over
                // from the left edge anyway.
                let end = row_bits.iter().rposition(|&b| b != 0).map_or(0, |e| e + 1);
                run_length(&row_bits[..end], &mut out);
            }
            band += 6;
            if band < y1 {
                out.push(b'-');
            }
        }
        out.extend_from_slice(b"\x1b\\");
        out
    }
}

/// Appends `bits` as sixel characters, repeating a run of four or more with `!`.
fn run_length(bits: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < bits.len() {
        let mut n = 1;
        while i + n < bits.len() && bits[i + n] == bits[i] {
            n += 1;
        }
        let c = bits[i] + 0x3f;
        if n >= 4 {
            out.extend_from_slice(format!("!{n}").as_bytes());
            out.push(c);
        } else {
            out.extend(std::iter::repeat_n(c, n));
        }
        i += n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x12 image in two colours: red on top, green below, from RGB definitions.
    #[test]
    fn decode_reads_sizes_colours_and_bands() {
        let bmp = decode(b"0;1;0", b"\"1;1;4;12#1;2;100;0;0#2;2;0;100;0#1!4~-#2~~~~");
        assert_eq!((bmp.width, bmp.height), (4, 12));
        assert_eq!(bmp.p2, 1);
        assert_eq!(bmp.palette[1], [255, 0, 0]);
        assert_eq!(bmp.palette[2], [0, 255, 0]);
        assert!(bmp.pixels[..24].iter().all(|&p| p == 2));
        assert!(bmp.pixels[24..].iter().all(|&p| p == 3));
    }

    /// The raster attributes can declare more than the data draws; the image keeps the
    /// declared size, as tmux pads an image to whole cells that way.
    #[test]
    fn decode_keeps_the_declared_size() {
        let bmp = decode(b"", b"\"1;1;10;20#0~");
        assert_eq!((bmp.width, bmp.height), (10, 20));
        assert_eq!(bmp.pixels[0], 1);
        assert_eq!(bmp.pixels[1], 0);
    }

    /// An HLS colour uses sixel's hue origin: blue at 0, red at 120, green at 240.
    #[test]
    fn hls_puts_blue_at_zero() {
        assert_eq!(hls(0, 50, 100), [0, 0, 255]);
        assert_eq!(hls(120, 50, 100), [255, 0, 0]);
        assert_eq!(hls(240, 50, 100), [0, 255, 0]);
    }

    /// Encoding a crop and decoding it again gives back exactly the cropped pixels.
    #[test]
    fn an_encoded_crop_decodes_to_the_same_pixels() {
        let mut data = b"\"1;1;30;18#1;2;100;0;0#2;2;0;0;100".to_vec();
        for band in 0..3 {
            data.extend_from_slice(b"#1!10~");
            data.extend_from_slice(b"#2!10~");
            data.extend_from_slice(if band % 2 == 0 { b"#1!10F" } else { b"#2!10w" });
            data.push(b'-');
        }
        let src = decode(b"0;1", &data);
        let (x, y, w, h) = (7, 5, 15, 9);
        let enc = src.encode(x, y, w, h);
        assert!(enc.starts_with(b"\x1bP0;1;0q\"1;1;15;9"));
        assert!(enc.ends_with(b"\x1b\\"));
        let body = &enc[b"\x1bP0;1;0q".len()..enc.len() - 2];
        let back = decode(b"0;1", body);
        assert_eq!((back.width, back.height), (w, h));
        for py in 0..h {
            for px in 0..w {
                let a = src.pixels[(y + py) * src.width + x + px];
                let b = back.pixels[py * w + px];
                let colour = |p: u16, bm: &Bitmap| (p > 0).then(|| bm.palette[usize::from(p - 1)]);
                assert_eq!(colour(a, &src), colour(b, &back), "pixel {px},{py}");
            }
        }
    }

    /// A crop past the bitmap's edge is clipped to it.
    #[test]
    fn encode_clips_to_the_bitmap() {
        let src = decode(b"", b"\"1;1;4;6#1!4~");
        assert!(src
            .encode(2, 0, 10, 10)
            .starts_with(b"\x1bP0;0;0q\"1;1;2;6"));
    }
}
