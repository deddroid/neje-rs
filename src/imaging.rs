//! Image preparation, ported from `LaserCarver.LaserImage` of the NEJE software through the
//! Python prototype, so the result is the same byte for byte.
//!
//! The functions keep the "odd" details of the original on purpose: Otsu counts the zero
//! padding bytes of rows aligned to 4 bytes, the error diffusion runs over the row edge,
//! and so on.

use std::path::Path;

use image::{DynamicImage, GrayImage, Luma, RgbImage, RgbaImage};

use crate::resample;

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("cannot open image {path}: {cause}")]
    Open { path: String, cause: image::ImageError },
}

/// How a photo becomes black and white.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Otsu threshold (`ToBW`), the default of the original program.
    #[default]
    Bw,
    /// NEJE error diffusion (`ToTremble`), for photos.
    Dither,
    /// Fixed threshold at 127.
    GrayThreshold,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Bw, Mode::Dither, Mode::GrayThreshold];
}

/// Opens any format the `image` crate reads (JPEG, PNG, BMP, GIF, TIFF, WebP). The format
/// comes from the content, not from the extension: the NEJE clipart are PNG files named
/// `.bmp`. Transparent pixels are laid on white (the original turned them black via GDI+).
pub fn load(path: &Path) -> Result<RgbImage, ImageError> {
    let open_error = |cause| ImageError::Open { path: path.display().to_string(), cause };
    let reader =
        image::ImageReader::open(path).and_then(|r| r.with_guessed_format()).map_err(|e| open_error(e.into()))?;
    Ok(flatten(reader.decode().map_err(open_error)?))
}

/// RGB version of `img`; images with an alpha channel are composited over opaque white with
/// Pillow's integer formula (`ImagingAlphaComposite`).
pub fn flatten(img: DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.into_rgb8();
    }
    let rgba: RgbaImage = img.into_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        image::Rgb([over_white(r, a), over_white(g, a), over_white(b, a)])
    })
}

/// One channel of `src` (alpha `a`) over an opaque white pixel, exactly as Pillow does it.
fn over_white(c: u8, a: u8) -> u8 {
    const PRECISION_BITS: u32 = 7;
    if a == 0 {
        return 255;
    }
    let (c, a) = (u32::from(c), u32::from(a));
    let dst_a = 255;
    let blend = dst_a * (255 - a);
    let outa255 = a * 255 + blend;
    let coef1 = a * 255 * 255 * (1 << PRECISION_BITS) / outa255;
    let coef2 = 255 * (1 << PRECISION_BITS) - coef1;
    let tmp = c * coef1 + 255 * coef2 + (0x80 << PRECISION_BITS);
    (((tmp >> 8) + tmp) >> 8 >> PRECISION_BITS) as u8
}

/// `ToEqualSmall`: the size after shrinking to fit `max_border` (C# integer divisions).
pub fn equal_small_size(w: u32, h: u32, max_border: u32) -> (u32, u32) {
    if w <= max_border && h <= max_border {
        (w, h)
    } else if w > h {
        (max_border, max_border * h / w)
    } else if w == h {
        (max_border, max_border)
    } else {
        (max_border * w / h, max_border)
    }
}

/// Shrinks to fit `max_border`, keeping the proportions (bilinear, like GDI+ `DrawImage`).
pub fn to_equal_small(img: &RgbImage, max_border: u32) -> RgbImage {
    let (w, h) = equal_small_size(img.width(), img.height(), max_border);
    if (w, h) == img.dimensions() {
        return img.clone();
    }
    let data =
        resample::resize_bilinear(img.as_raw(), img.width() as usize, img.height() as usize, 3, w as usize, h as usize);
    RgbImage::from_raw(w, h, data).expect("resize returns w*h*3 bytes")
}

/// `ToGray` + `To8bppImage`: Y = 0.299 R + 0.587 G + 0.114 B, +0.5 and truncated.
pub fn to_gray(img: &RgbImage) -> GrayImage {
    GrayImage::from_fn(img.width(), img.height(), |x, y| {
        let [r, g, b] = img.get_pixel(x, y).0;
        Luma([(f64::from(r) * 0.299 + f64::from(g) * 0.587 + f64::from(b) * 0.114 + 0.5) as u8])
    })
}

/// Copy of `img` in a buffer whose rows are padded with zeros to 4 bytes, like the
/// `BitmapData` of an 8-bit GDI+ bitmap. Returns the buffer and the stride.
fn to_strided(img: &GrayImage) -> (Vec<u8>, usize) {
    let w = img.width() as usize;
    let stride = (w + 3) & !3;
    let mut buf = vec![0u8; stride * img.height() as usize];
    for (y, row) in img.as_raw().chunks_exact(w.max(1)).enumerate().take(img.height() as usize) {
        buf[y * stride..y * stride + w].copy_from_slice(row);
    }
    (buf, stride)
}

fn from_strided(buf: &[u8], stride: usize, w: u32, h: u32) -> GrayImage {
    GrayImage::from_fn(w, h, |x, y| Luma([buf[y as usize * stride + x as usize]]))
}

/// `GetOSTUThreshold`, copied literally, including the loop that should look for the last
/// non-empty level but tests `HistGram[i]` instead (so the upper end stays 255).
pub fn otsu_threshold(hist: &[u64; 256]) -> u8 {
    let Some(first) = hist.iter().position(|&n| n != 0) else {
        return 0; // empty image: C# would throw IndexOutOfRange
    };
    let mut last = 255;
    while last > first && hist[first] == 0 {
        last -= 1;
    }
    if last == first {
        return last as u8;
    }
    if first + 1 == last {
        return first as u8;
    }
    let total: u64 = hist[first..=last].iter().sum();
    let sum_all: u64 = (first..=last).map(|j| hist[j] * j as u64).sum();
    let (mut acc_n, mut acc_w) = (0u64, 0u64);
    let mut best = -1.0f64;
    let mut result = 0;
    for (j, &n) in hist.iter().enumerate().take(last).skip(first) {
        acc_n += n;
        let rest_n = total - acc_n;
        let w0 = acc_n as f64 / total as f64;
        let w1 = rest_n as f64 / total as f64;
        acc_w += n * j as u64;
        let rest_w = sum_all - acc_w;
        let m0 = if acc_n != 0 { acc_w as f64 / acc_n as f64 } else { f64::NAN };
        let m1 = if rest_n != 0 { rest_w as f64 / rest_n as f64 } else { f64::NAN };
        let var = w0 * w1 * (m0 - m1) * (m0 - m1);
        // NaN > x is false, as in C#.
        if var > best {
            best = var;
            result = j;
        }
    }
    result as u8
}

/// `ToBW`: Otsu threshold over the whole padded buffer (padding bytes included).
pub fn bw_otsu(img: &GrayImage) -> GrayImage {
    let (mut buf, stride) = to_strided(img);
    let mut hist = [0u64; 256];
    for &v in &buf {
        hist[usize::from(v)] += 1;
    }
    let t = otsu_threshold(&hist);
    for v in &mut buf {
        *v = if *v > t { 255 } else { 0 };
    }
    from_strided(&buf, stride, img.width(), img.height())
}

/// `ToTremble`: NEJE error diffusion (3/8 right, 3/8 below, 2/8 diagonal).
///
/// Faithful copy: min and max come from the first W×H bytes of the padded buffer (padding
/// included), the threshold is (min+max)/2, pixels above 230 become white without error,
/// and the error is measured against max/min.
pub fn dither(img: &GrayImage) -> GrayImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let (mut a, stride) = to_strided(img);
    let n = a.len();
    let (lo, hi) = a[..w * h].iter().fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let (lo, hi) = (i32::from(lo), i32::from(hi));
    let mid = (lo + hi) / 2;
    let spread = |a: &mut [u8], p: usize, delta: i32| {
        if p < n {
            a[p] = (i32::from(a[p]) + delta).clamp(0, 255) as u8;
        }
    };
    for k in 0..h {
        for l in 0..w {
            let p = k * stride + l;
            let v = i32::from(a[p]);
            let err = if v > 230 {
                a[p] = 255;
                0
            } else if v > mid {
                a[p] = 255;
                v - hi
            } else {
                a[p] = 0;
                v - lo
            };
            // Rust integer division truncates towards zero, like C#.
            spread(&mut a, p + 1, err * 3 / 8);
            spread(&mut a, p + stride, err * 3 / 8);
            spread(&mut a, p + stride + 1, err * 2 / 8);
        }
    }
    from_strided(&a, stride, img.width(), img.height())
}

/// `pictureBox_show_DragDrop`: shrink to `base_size`, grey, then black and white.
pub fn prepare(img: &RgbImage, base_size: u32, mode: Mode) -> GrayImage {
    let gray = to_gray(&to_equal_small(img, base_size));
    match mode {
        Mode::Bw => bw_otsu(&gray),
        Mode::Dither => dither(&gray),
        Mode::GrayThreshold => {
            let mut out = gray;
            for p in out.pixels_mut() {
                p.0[0] = if p.0[0] > 127 { 255 } else { 0 };
            }
            out
        }
    }
}

/// Empty (white) work area.
pub fn new_base(base_size: u32) -> GrayImage {
    GrayImage::from_pixel(base_size, base_size, Luma([255]))
}

/// Copies `img` onto `dst` with its top left corner at (`x`, `y`), clipping at the edges.
pub fn paste(dst: &mut GrayImage, img: &GrayImage, x: i64, y: i64) {
    paste_where(dst, img, x, y, Some);
}

fn paste_where(dst: &mut GrayImage, img: &GrayImage, x: i64, y: i64, pixel: impl Fn(u8) -> Option<u8>) {
    for (sx, sy, p) in img.enumerate_pixels() {
        let (dx, dy) = (x + i64::from(sx), y + i64::from(sy));
        if (0..i64::from(dst.width())).contains(&dx)
            && (0..i64::from(dst.height())).contains(&dy)
            && let Some(v) = pixel(p.0[0])
        {
            dst.put_pixel(dx as u32, dy as u32, Luma([v]));
        }
    }
}

/// `ToCenter`: `img` pasted at the centre of a copy of `base` (integer division).
pub fn center_on(base: &GrayImage, img: &GrayImage) -> GrayImage {
    let mut out = base.clone();
    let x = (i64::from(base.width()) - i64::from(img.width())).div_euclid(2);
    let y = (i64::from(base.height()) - i64::from(img.height())).div_euclid(2);
    paste(&mut out, img, x, y);
    out
}

/// `button_Route_Click`: 90° clockwise.
pub fn rotate90(base: &GrayImage) -> GrayImage {
    image::imageops::rotate90(base)
}

/// `button_small_Click`: shrink to 90 %, black and white again, centre on a new area.
pub fn shrink(base: &GrayImage) -> GrayImage {
    let n = base.width().max(base.height()) * 9 / 10;
    let rgb = DynamicImage::ImageLuma8(base.clone()).into_rgb8();
    let small = bw_otsu(&to_gray(&to_equal_small(&rgb, n)));
    center_on(&new_base(base.width()), &small)
}

/// `pictureBox_show_Click`: pastes the black pixels of `img` centred on (`cx`, `cy`).
pub fn place(base: &GrayImage, img: &GrayImage, cx: i64, cy: i64) -> GrayImage {
    let mut out = base.clone();
    let x = cx - i64::from(img.width()).div_euclid(2);
    let y = cy - i64::from(img.height()).div_euclid(2);
    paste_where(&mut out, img, x, y, |v| (v < 128).then_some(0));
    out
}

/// Fraction of pixels that will be burnt (anything that is not pure white).
pub fn black_ratio(img: &GrayImage) -> f64 {
    let total = img.pixels().len().max(1);
    let white = img.pixels().filter(|p| p.0[0] == 255).count();
    1.0 - white as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_small() {
        assert_eq!(equal_small_size(1000, 500, 490), (490, 245));
        assert_eq!(equal_small_size(500, 1000, 490), (245, 490));
        assert_eq!(equal_small_size(800, 800, 490), (490, 490));
        assert_eq!(equal_small_size(100, 50, 490), (100, 50));
    }

    #[test]
    fn otsu_two_levels() {
        let im = GrayImage::from_fn(40, 40, |x, _| Luma([if x < 20 { 30 } else { 200 }]));
        let bw = bw_otsu(&im);
        assert_eq!(bw.get_pixel(5, 5).0[0], 0);
        assert_eq!(bw.get_pixel(30, 5).0[0], 255);
    }

    #[test]
    fn rotate_clockwise() {
        let mut im = new_base(490);
        for y in 10..40 {
            for x in 10..60 {
                im.put_pixel(x, y, Luma([0]));
            }
        }
        assert_eq!(rotate90(&im).get_pixel(480 - 25, 30).0[0], 0);
    }

    #[test]
    fn transparent_pixels_become_white() {
        assert_eq!(over_white(0, 0), 255);
        assert_eq!(over_white(0, 255), 0);
        assert_eq!(over_white(10, 255), 10);
    }

    #[test]
    fn place_only_copies_black() {
        let base = new_base(10);
        let stamp = GrayImage::from_fn(3, 3, |x, _| Luma([if x == 1 { 0 } else { 200 }]));
        let out = place(&base, &stamp, 5, 5);
        assert_eq!(out.get_pixel(5, 5).0[0], 0);
        assert_eq!(out.get_pixel(4, 5).0[0], 255);
    }
}
