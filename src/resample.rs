//! Bilinear resize identical to Pillow's `Image.resize(size, Image.BILINEAR)` for 8-bit
//! images (`libImaging/Resample.c`): a separable triangle filter whose support grows with
//! the reduction factor, fixed-point coefficients with 22 fractional bits and the same pass
//! order. The Python prototype uses Pillow, so matching it byte for byte needs this port.

const PRECISION_BITS: u32 = 32 - 8 - 2;

fn bilinear_filter(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 { 1.0 - x } else { 0.0 }
}

/// Coefficients for one axis: for each output pixel the first input index, the number of
/// inputs used and their fixed-point weights (`ksize` slots per output pixel).
struct Coeffs {
    ksize: usize,
    bounds: Vec<(usize, usize)>,
    weights: Vec<i32>,
}

/// `precompute_coeffs` + `normalize_coeffs_8bpc` with box = (0, in_size).
fn coeffs(in_size: usize, out_size: usize) -> Coeffs {
    let scale = in_size as f32 as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = filterscale; // BILINEAR support is 1.0
    let ksize = support.ceil() as usize * 2 + 1;
    let inv_filterscale = 1.0 / filterscale;
    let mut bounds = Vec::with_capacity(out_size);
    let mut weights = vec![0i32; out_size * ksize];
    let mut k = vec![0f64; ksize];
    for xx in 0..out_size {
        let center = (xx as f64 + 0.5) * scale;
        // C truncates towards zero, like `as`.
        let xmin = ((center - support + 0.5) as i32).max(0) as usize;
        let xmax = ((center + support + 0.5) as i32).min(in_size as i32) as usize - xmin;
        let mut ww = 0.0;
        for (x, slot) in k.iter_mut().enumerate().take(xmax) {
            let w = bilinear_filter(((x + xmin) as f64 - center + 0.5) * inv_filterscale);
            *slot = w;
            ww += w;
        }
        for (x, &w) in k.iter().enumerate().take(xmax) {
            let w = if ww != 0.0 { w / ww } else { w };
            let fixed = w * f64::from(1u32 << PRECISION_BITS);
            weights[xx * ksize + x] = if w < 0.0 { (-0.5 + fixed) as i32 } else { (0.5 + fixed) as i32 };
        }
        bounds.push((xmin, xmax));
    }
    Coeffs { ksize, bounds, weights }
}

fn clip8(v: i32) -> u8 {
    (v >> PRECISION_BITS).clamp(0, 255) as u8
}

const HALF: i32 = 1 << (PRECISION_BITS - 1);

/// Horizontal pass on rows `first_row..first_row + rows` of `src`.
fn horizontal(src: &[u8], width: usize, channels: usize, first_row: usize, rows: usize, c: &Coeffs) -> Vec<u8> {
    let out_w = c.bounds.len();
    let mut out = vec![0u8; out_w * rows * channels];
    for yy in 0..rows {
        let line = &src[(yy + first_row) * width * channels..][..width * channels];
        for (xx, &(xmin, xmax)) in c.bounds.iter().enumerate() {
            let k = &c.weights[xx * c.ksize..][..xmax];
            for ch in 0..channels {
                let mut ss = HALF;
                for (x, &w) in k.iter().enumerate() {
                    ss += i32::from(line[(x + xmin) * channels + ch]) * w;
                }
                out[(yy * out_w + xx) * channels + ch] = clip8(ss);
            }
        }
    }
    out
}

/// Vertical pass; `row_offset` is subtracted from the bounds (rows skipped by the
/// horizontal pass that ran first).
fn vertical(src: &[u8], width: usize, channels: usize, row_offset: usize, c: &Coeffs) -> Vec<u8> {
    let out_h = c.bounds.len();
    let row_len = width * channels;
    let mut out = vec![0u8; row_len * out_h];
    for (yy, &(ymin, ymax)) in c.bounds.iter().enumerate() {
        let k = &c.weights[yy * c.ksize..][..ymax];
        let ymin = ymin - row_offset;
        for i in 0..row_len {
            let mut ss = HALF;
            for (y, &w) in k.iter().enumerate() {
                ss += i32::from(src[(y + ymin) * row_len + i]) * w;
            }
            out[yy * row_len + i] = clip8(ss);
        }
    }
    out
}

/// Resizes interleaved 8-bit pixels (`channels` per pixel) from `w`×`h` to `out_w`×`out_h`.
pub fn resize_bilinear(src: &[u8], w: usize, h: usize, channels: usize, out_w: usize, out_h: usize) -> Vec<u8> {
    assert_eq!(src.len(), w * h * channels, "pixel buffer does not match {w}x{h}x{channels}");
    let need_horizontal = out_w != w;
    let need_vertical = out_h != h;
    // ImagingResampleInner: vertical pass first when the height shrinks more than twice as
    // much as the width.
    let (dh, dw) = (h as i64 - out_h as i64, w as i64 - out_w as i64);
    let horizontal_first = !(dh > 0 && dh > dw * 2);
    let vert = ((need_horizontal && horizontal_first) || need_vertical).then(|| coeffs(h, out_h));
    match (horizontal_first, vert) {
        (true, vert) => {
            let mut pixels = None;
            let mut offset = 0;
            if need_horizontal {
                let vc = vert.as_ref().expect("vertical coefficients are computed when the horizontal pass runs first");
                let first = vc.bounds[0].0;
                let (lmin, lmax) = vc.bounds[out_h - 1];
                let last = lmin + lmax;
                offset = first;
                pixels = Some(horizontal(src, w, channels, first, last - first, &coeffs(w, out_w)));
            }
            if need_vertical {
                let vc = vert.as_ref().expect("vertical coefficients exist when the height changes");
                let width = if need_horizontal { out_w } else { w };
                let input = pixels.as_deref().unwrap_or(src);
                pixels = Some(vertical(input, width, channels, offset, vc));
            }
            pixels.unwrap_or_else(|| src.to_vec())
        }
        (false, vert) => {
            let vc = vert.expect("the vertical pass runs first only when the height shrinks");
            let pixels = vertical(src, w, channels, 0, &vc);
            if need_horizontal { horizontal(&pixels, w, channels, 0, out_h, &coeffs(w, out_w)) } else { pixels }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_size_is_a_copy() {
        let src: Vec<u8> = (0..=255).collect();
        assert_eq!(resize_bilinear(&src, 16, 16, 1, 16, 16), src);
    }

    #[test]
    fn uniform_image_stays_uniform() {
        let src = vec![77u8; 300 * 200 * 3];
        let out = resize_bilinear(&src, 300, 200, 3, 123, 45);
        assert_eq!(out.len(), 123 * 45 * 3);
        assert!(out.iter().all(|&v| v == 77));
    }
}
