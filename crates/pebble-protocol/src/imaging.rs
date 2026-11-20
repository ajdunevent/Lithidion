//! Images the watch asks the phone for (PROTOCOL.md section 22): album art and the pictures in
//! notifications. The watch names the size it will show; the phone answers in chunks of a 4-bit
//! palette image, or says it has none. Little-endian throughout.

use crate::wire::{Reader, Result};
use std::fmt;
use uuid::Uuid;

const CMD_REQUEST: u8 = 0x01;
const CMD_RESPONSE: u8 = 0x02;

pub const TYPE_ALBUM_ART: u8 = 0x00;
pub const TYPE_NOTIFICATION: u8 = 0x01;

pub const FORMAT_4BIT_PALETTE: u8 = 0x02;

const FLAG_FIRST: u8 = 0x01;
const FLAG_LAST: u8 = 0x02;
const FLAG_NO_IMAGE: u8 = 0x04;
const FLAG_UNSUPPORTED: u8 = 0x08;

/// The watch's limits: it drops anything larger.
pub const MAX_DIM: u16 = 300;
pub const MAX_BYTES: usize = 40 * 1024;
/// Pixel bytes per chunk.
const CHUNK: usize = 1000;

/// Height over width in sixteenths, as the watch bounds it.
const MIN_ASPECT: u8 = 4;
const MAX_ASPECT: u8 = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub token: u8,
    pub kind: u8,
    pub format: u8,
    pub width: u16,
    pub height: u16,
    /// The notification's item, for notification pictures.
    pub item: Option<Uuid>,
}

pub fn parse_request(payload: &[u8]) -> Result<Option<Request>> {
    let mut r = Reader::new(payload);
    if r.u8()? != CMD_REQUEST {
        return Ok(None);
    }
    let token = r.u8()?;
    let kind = r.u8()?;
    let format = r.u8()?;
    let width = r.u16_le()?;
    let height = r.u16_le()?;
    let item = if kind == TYPE_NOTIFICATION { Some(r.uuid()?) } else { None };
    Ok(Some(Request { token, kind, format, width, height, item }))
}

fn header(token: u8, kind: u8, flags: u8, offset: u32, len: u16) -> Vec<u8> {
    let mut out = vec![CMD_RESPONSE, token, flags | (kind << 4)];
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out
}

/// The phone has no image for this request.
pub fn no_image(token: u8, kind: u8) -> Vec<u8> {
    header(token, kind, FLAG_NO_IMAGE, 0, 0)
}

/// The phone never serves this type; the watch stops asking for the rest of the connection.
pub fn unsupported(token: u8, kind: u8) -> Vec<u8> {
    header(token, kind, FLAG_UNSUPPORTED, 0, 0)
}

/// An image as the watch draws it: up to 16 colours, two pixels a byte, the left one in the high
/// nibble, rows padded to a whole byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paletted {
    pub width: u16,
    pub height: u16,
    /// GColor8 entries: two bits each of alpha, red, green, and blue.
    pub palette: Vec<u8>,
    pub pixels: Vec<u8>,
}

/// The response chunks for an image.
pub fn response(token: u8, kind: u8, image: &Paletted) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> = image.pixels.chunks(CHUNK).collect();
    let last = chunks.len() - 1;
    chunks
        .iter()
        .enumerate()
        .map(|(i, pixels)| {
            let flags = if i == 0 { FLAG_FIRST } else { 0 } | if i == last { FLAG_LAST } else { 0 };
            let mut out = header(token, kind, flags, (i * CHUNK) as u32, pixels.len() as u16);
            if i == 0 {
                out.extend_from_slice(&image.width.to_le_bytes());
                out.extend_from_slice(&image.height.to_le_bytes());
                out.push(FORMAT_4BIT_PALETTE);
                out.push(image.palette.len() as u8);
                out.extend_from_slice(&image.palette);
            }
            out.extend_from_slice(pixels);
            out
        })
        .collect()
}

/// A picture as it came: eight bits each of red, green, blue, and alpha.
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl fmt::Debug for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rgba({}×{})", self.width, self.height)
    }
}

impl Rgba {
    /// Opaque RGB over white, at a pixel.
    fn rgb(&self, x: u32, y: u32) -> [f32; 3] {
        let i = ((y * self.width + x) * 4) as usize;
        let p = &self.pixels[i..i + 4];
        let a = p[3] as f32 / 255.0;
        [0, 1, 2].map(|c| p[c] as f32 * a + 255.0 * (1.0 - a))
    }

    /// Scaled down, never up, so its longer side is at most `max`.
    pub fn bounded(&self, max: u32) -> Rgba {
        let scale = (max as f32 / self.width.max(self.height) as f32).min(1.0);
        let (w, h) = (((self.width as f32 * scale).round() as u32).max(1), ((self.height as f32 * scale).round() as u32).max(1));
        if (w, h) == (self.width, self.height) {
            return self.clone();
        }
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        let (sx, sy) = (self.width as f32 / w as f32, self.height as f32 / h as f32);
        for y in 0..h {
            for x in 0..w {
                pixels.extend(self.area(x as f32 * sx, y as f32 * sy, sx, sy, true));
            }
        }
        Rgba { width: w, height: h, pixels }
    }

    /// The mean of a region, by a grid of samples. Straight RGBA when `rgba`, else RGB over white
    /// with alpha 255.
    fn area(&self, x0: f32, y0: f32, w: f32, h: f32, rgba: bool) -> [u8; 4] {
        let (nx, ny) = (w.ceil().clamp(1.0, 8.0) as u32, h.ceil().clamp(1.0, 8.0) as u32);
        let mut sum = [0f32; 4];
        for j in 0..ny {
            for i in 0..nx {
                let x = ((x0 + (i as f32 + 0.5) * w / nx as f32) as u32).min(self.width - 1);
                let y = ((y0 + (j as f32 + 0.5) * h / ny as f32) as u32).min(self.height - 1);
                if rgba {
                    let k = ((y * self.width + x) * 4) as usize;
                    for (s, v) in sum.iter_mut().zip(&self.pixels[k..k + 4]) {
                        *s += *v as f32;
                    }
                } else {
                    for (s, v) in sum.iter_mut().zip(self.rgb(x, y)) {
                        *s += v;
                    }
                    sum[3] += 255.0;
                }
            }
        }
        let n = (nx * ny) as f32;
        sum.map(|s| (s / n).round() as u8)
    }
}

/// Height over width in sixteenths, within what the watch accepts.
pub fn aspect_sixteenths(width: u32, height: u32) -> u8 {
    let a = (height as f32 * 16.0 / width.max(1) as f32).round();
    (a.clamp(MIN_ASPECT as f32, MAX_ASPECT as f32)) as u8
}

/// The size to send for a box the watch reserved: the box's shape, within the watch's limits, and
/// at most twice the picture's own size so small pictures are not blown up into blocks.
fn fitted_size(src: &Rgba, box_w: u16, box_h: u16) -> (u16, u16) {
    let (bw, bh) = (box_w.max(1) as f32, box_h.max(1) as f32);
    let mut scale = (MAX_DIM as f32 / bw).min(MAX_DIM as f32 / bh).min(1.0);
    // Cropped to the box's shape, the picture's size along the box's width.
    let crop_w = (src.width as f32).min(src.height as f32 * bw / bh);
    scale = scale.min(2.0 * crop_w / bw);
    let bytes = |s: f32| ((bw * s).round() as usize).div_ceil(2) * (bh * s).round() as usize;
    while scale > 0.0 && bytes(scale) > MAX_BYTES {
        scale *= 0.95;
    }
    (((bw * scale).round() as u16).max(1), ((bh * scale).round() as u16).max(1))
}

/// The picture for a box the watch reserved: cropped to the box's shape from the centre, scaled,
/// and reduced to the watch's colours.
pub fn encode(src: &Rgba, box_w: u16, box_h: u16) -> Paletted {
    let (w, h) = fitted_size(src, box_w, box_h);
    let target = w as f32 / h as f32;
    let (sw, sh) = (src.width as f32, src.height as f32);
    let (cw, ch) = if sw / sh > target { (sh * target, sh) } else { (sw, sw / target) };
    let (cx, cy) = ((sw - cw) / 2.0, (sh - ch) / 2.0);
    let (fx, fy) = (cw / w as f32, ch / h as f32);
    let mut rgb = Vec::with_capacity(w as usize * h as usize);
    for y in 0..h {
        for x in 0..w {
            let p = src.area(cx + x as f32 * fx, cy + y as f32 * fy, fx, fy, false);
            rgb.push([p[0], p[1], p[2]]);
        }
    }
    quantize(&rgb, w, h)
}

/// Up to 16 of the watch's 64 colours, picked one at a time for the least remaining error over
/// the picture, then Floyd–Steinberg dithering.
fn quantize(rgb: &[[u8; 3]], w: u16, h: u16) -> Paletted {
    // The picture as weighted bins of four bits a channel, each at the mean of its pixels.
    let mut bins = vec![(0f32, [0f32; 3]); 4096];
    for p in rgb {
        let b = &mut bins[((p[0] as usize >> 4) << 8) | ((p[1] as usize >> 4) << 4) | (p[2] as usize >> 4)];
        b.0 += 1.0;
        for (s, v) in b.1.iter_mut().zip(p) {
            *s += *v as f32;
        }
    }
    let bins: Vec<(f32, [f32; 3])> = bins.into_iter().filter(|b| b.0 > 0.0).map(|(n, s)| (n, s.map(|v| v / n))).collect();
    let lattice: Vec<[u8; 3]> = (0..64u8).map(|i| [i >> 4, (i >> 2) & 3, i & 3]).collect();
    let at = |l: &[u8; 3]| l.map(|v| v as f32 * 85.0);
    let mut levels: Vec<[u8; 3]> = Vec::new();
    let mut nearest = vec![f32::INFINITY; bins.len()];
    while levels.len() < 16 {
        let best = lattice
            .iter()
            .filter(|l| !levels.contains(l))
            .map(|l| {
                let c = at(l);
                let cost: f32 = bins.iter().zip(&nearest).map(|((n, m), d)| n * d.min(dist(&c, m))).sum();
                (l, cost)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let Some((l, cost)) = best else { break };
        let before: f32 = bins.iter().zip(&nearest).map(|((n, _), d)| if d.is_finite() { n * d } else { f32::INFINITY }).sum();
        if cost >= before {
            break;
        }
        let c = at(l);
        for (d, (_, m)) in nearest.iter_mut().zip(&bins) {
            *d = d.min(dist(&c, m));
        }
        levels.push(*l);
    }
    let colors: Vec<[f32; 3]> = levels.iter().map(|l| l.map(|v| v as f32 * 85.0)).collect();
    let palette = levels.iter().map(|l| 0b1100_0000 | (l[0] << 4) | (l[1] << 2) | l[2]).collect();

    let (w, h) = (w as usize, h as usize);
    let row = w.div_ceil(2);
    let mut pixels = vec![0u8; row * h];
    let mut err = vec![[0f32; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let k = y * w + x;
            let want = [0, 1, 2].map(|c| (rgb[k][c] as f32 + err[k][c]).clamp(0.0, 255.0));
            let (index, got) =
                colors.iter().enumerate().min_by(|a, b| dist(a.1, &want).total_cmp(&dist(b.1, &want))).map(|(i, c)| (i as u8, *c)).unwrap();
            pixels[y * row + x / 2] |= if x % 2 == 0 { index << 4 } else { index };
            let e = [0, 1, 2].map(|c| (want[c] - got[c]) * DIFFUSION);
            let mut spread = |dx: isize, dy: usize, f: f32| {
                let nx = x as isize + dx;
                if nx >= 0 && (nx as usize) < w && y + dy < h {
                    let n = (y + dy) * w + nx as usize;
                    for c in 0..3 {
                        err[n][c] += e[c] * f;
                    }
                }
            };
            spread(1, 0, 7.0 / 16.0);
            spread(-1, 1, 3.0 / 16.0);
            spread(0, 1, 5.0 / 16.0);
            spread(1, 1, 1.0 / 16.0);
        }
    }
    Paletted { width: w as u16, height: h as u16, palette, pixels }
}

/// Less than all of the error is carried on: at 64 colours, full diffusion speckles flat areas.
const DIFFUSION: f32 = 0.75;

fn dist(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    (0..3).map(|c| (a[c] - b[c]).powi(2)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Rgba {
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                pixels.extend([(x * 255 / w) as u8, (y * 255 / h) as u8, 128, 255]);
            }
        }
        Rgba { width: w, height: h, pixels }
    }

    #[test]
    fn request_for_a_notification() {
        let mut p = vec![0x01, 7, 0x01, 0x02, 180, 0, 120, 0];
        let id = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        p.extend_from_slice(id.as_bytes());
        let r = parse_request(&p).unwrap().unwrap();
        assert_eq!(r, Request { token: 7, kind: 1, format: 2, width: 180, height: 120, item: Some(id) });
    }

    #[test]
    fn chunks_reassemble_to_the_image() {
        let img = encode(&gradient(400, 300), 180, 135);
        assert_eq!((img.width, img.height), (180, 135));
        assert!(img.palette.len() <= 16 && img.palette.iter().all(|c| c >> 6 == 3));
        assert_eq!(img.pixels.len(), 90 * 135);
        let chunks = response(9, TYPE_NOTIFICATION, &img);
        let mut got = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(&c[..2], &[0x02, 9]);
            let flags = c[2];
            assert_eq!(flags >> 4, 1);
            assert_eq!(flags & FLAG_FIRST != 0, i == 0);
            assert_eq!(flags & FLAG_LAST != 0, i == chunks.len() - 1);
            assert_eq!(u32::from_le_bytes(c[3..7].try_into().unwrap()) as usize, got.len());
            let len = u16::from_le_bytes(c[7..9].try_into().unwrap()) as usize;
            let start = if i == 0 { 9 + 6 + img.palette.len() } else { 9 };
            assert_eq!(c.len() - start, len);
            got.extend_from_slice(&c[start..]);
        }
        assert_eq!(got, img.pixels);
    }

    #[test]
    fn limits_and_shapes() {
        // Too tall for the watch: scaled down to fit, keeping the box's shape.
        let (w, h) = fitted_size(&gradient(1000, 1000), 240, 360);
        assert!(h <= MAX_DIM && (w as f32 / h as f32 - 240.0 / 360.0).abs() < 0.01);
        assert!((w as usize).div_ceil(2) * h as usize <= MAX_BYTES);
        // A small avatar is not blown up more than twice.
        assert_eq!(fitted_size(&gradient(48, 48), 180, 180), (96, 96));
        assert_eq!(aspect_sixteenths(400, 300), 12);
        assert_eq!(aspect_sixteenths(1000, 100), MIN_ASPECT);
        assert_eq!(aspect_sixteenths(100, 1000), MAX_ASPECT);
        let b = gradient(1200, 600).bounded(260);
        assert_eq!((b.width, b.height), (260, 130));
    }

    #[test]
    fn transparency_is_over_white() {
        let clear = Rgba { width: 2, height: 2, pixels: vec![0; 16] };
        let img = encode(&clear, 2, 2);
        assert_eq!(img.palette, vec![0xFF]);
        assert_eq!(img.pixels, vec![0, 0]);
    }
}
