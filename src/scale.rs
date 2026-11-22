//! Scaling what is on the work area to a size. Every size is computed from the same
//! starting point (the [`Baseline`]), so repeated changes never add their blur together.

use image::{GrayImage, RgbImage};

use crate::imaging::{self, Mode};

/// Bounding box of the ink: x, y, width, height in pixels of the work area.
pub type Bounds = (u32, u32, u32, u32);

enum Source {
    /// The part of the opened picture that holds the ink, converted again at every size so
    /// that dithering is done at the final resolution.
    Picture { crop: RgbImage, mode: Mode },
    /// The ink of the work area as it was.
    Bitmap(GrayImage),
}

/// The work area at 100 %, to scale from.
pub struct Baseline {
    area: u32,
    bounds: Bounds,
    source: Source,
}

impl Baseline {
    /// From the work area as it is. `None` when there is nothing to burn.
    pub fn bitmap(base: &GrayImage) -> Option<Self> {
        let bounds = imaging::ink_bounds(base)?;
        Some(Baseline { area: base.width(), bounds, source: Source::Bitmap(imaging::crop(base, bounds)) })
    }

    /// From an opened picture laid on an empty `area`² work area with `mode`, exactly as
    /// [`imaging::prepare`] and [`imaging::center_on`] lay it. `None` when it has no ink.
    pub fn picture(original: &RgbImage, mode: Mode, area: u32) -> Option<Self> {
        let prepared = imaging::prepare(original, area, mode);
        let (ix, iy, iw, ih) = imaging::ink_bounds(&prepared)?;
        let (pw, ph) = prepared.dimensions();
        // Same position as `imaging::center_on`; the prepared picture never exceeds the area.
        let (ox, oy) = ((area - pw) / 2, (area - ph) / 2);
        // The ink box in the pixels of the original, rounded outwards.
        let span = |start: u32, len: u32, ratio: f64, max: u32| {
            let first = ((f64::from(start) * ratio).floor() as u32).min(max - 1);
            let end = ((f64::from(start + len) * ratio).ceil() as u32).clamp(first + 1, max);
            (first, end - first)
        };
        let (cx, cw) = span(ix, iw, f64::from(original.width()) / f64::from(pw), original.width());
        let (cy, ch) = span(iy, ih, f64::from(original.height()) / f64::from(ph), original.height());
        let mut crop = image::imageops::crop_imm(original, cx, cy, cw, ch).to_image();
        // Nothing is ever drawn larger than the area: a crop of a big photo is shrunk once here
        // rather than resampled at full resolution at every size.
        let (sw, sh) = imaging::equal_small_size(cw, ch, area);
        if sw > 0 && sh > 0 {
            crop = imaging::to_equal_small(&crop, area);
        }
        Some(Baseline { area, bounds: (ox + ix, oy + iy, iw, ih), source: Source::Picture { crop, mode } })
    }

    /// Size of the ink at 100 %.
    pub fn ink_size(&self) -> (u32, u32) {
        (self.bounds.2, self.bounds.3)
    }

    /// `factor` limited so that the ink is at least one pixel and fits the work area.
    pub fn clamp_factor(&self, factor: f64) -> f64 {
        let (w, h) = (f64::from(self.bounds.2), f64::from(self.bounds.3));
        let area = f64::from(self.area);
        factor.clamp((1.0 / w).max(1.0 / h), (area / w).min(area / h))
    }

    /// Size of the ink scaled by `factor` (limited by [`Baseline::clamp_factor`]).
    pub fn size(&self, factor: f64) -> (u32, u32) {
        let factor = self.clamp_factor(factor);
        let area = f64::from(self.area);
        let side = |len: u32| (f64::from(len) * factor).round().clamp(1.0, area) as u32;
        (side(self.bounds.2), side(self.bounds.3))
    }

    /// The work area with the ink scaled to `w`×`h` pixels around the centre it had at 100 %.
    pub fn render(&self, w: u32, h: u32) -> GrayImage {
        let ink = match &self.source {
            Source::Picture { crop, mode } => imaging::prepare_sized(crop, w, h, *mode),
            Source::Bitmap(ink) => imaging::scale_bitmap(ink, w, h),
        };
        let (x, y, bw, bh) = self.bounds;
        let cx = f64::from(x) + f64::from(bw) / 2.0;
        let cy = f64::from(y) + f64::from(bh) / 2.0;
        imaging::place_inside(self.area, &ink, cx, cy)
    }
}

#[cfg(test)]
mod tests {
    use image::{Luma, Rgb};

    use super::*;

    fn block_on(area: u32, (x, y, w, h): Bounds) -> GrayImage {
        let mut im = imaging::new_base(area);
        for yy in y..y + h {
            for xx in x..x + w {
                im.put_pixel(xx, yy, Luma([0]));
            }
        }
        im
    }

    #[test]
    fn empty_area_has_no_baseline() {
        assert!(Baseline::bitmap(&imaging::new_base(100)).is_none());
        let white = RgbImage::from_pixel(10, 10, Rgb([255, 255, 255]));
        assert!(Baseline::picture(&white, Mode::Bw, 100).is_none());
    }

    #[test]
    fn bitmap_scales_around_its_centre() {
        let base = block_on(490, (100, 100, 20, 10));
        let baseline = Baseline::bitmap(&base).unwrap();
        assert_eq!(baseline.ink_size(), (20, 10));
        assert_eq!(imaging::ink_bounds(&baseline.render(40, 20)), Some((90, 95, 40, 20)));
        assert_eq!(baseline.render(20, 10), base);
    }

    #[test]
    fn sizes_keep_proportions_and_stay_in_the_area() {
        let baseline = Baseline::bitmap(&block_on(490, (100, 100, 20, 10))).unwrap();
        assert_eq!(baseline.size(2.5), (50, 25));
        assert_eq!(baseline.size(3.0), (60, 30));
        assert_eq!(baseline.clamp_factor(1000.0), 24.5);
        assert_eq!(baseline.size(1000.0), (490, 245));
        assert_eq!(baseline.size(0.0), (2, 1));
    }

    #[test]
    fn scaled_ink_is_pushed_back_inside() {
        let baseline = Baseline::bitmap(&block_on(490, (0, 0, 20, 10))).unwrap();
        assert_eq!(imaging::ink_bounds(&baseline.render(100, 50)), Some((0, 0, 100, 50)));
    }

    #[test]
    fn picture_is_converted_again_at_the_new_size() {
        let mut rgb = RgbImage::from_pixel(100, 50, Rgb([255, 255, 255]));
        for y in 20..30 {
            for x in 40..60 {
                rgb.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let baseline = Baseline::picture(&rgb, Mode::Bw, 490).unwrap();
        // The 100x50 picture sits in the middle of the area, at (195, 220).
        assert_eq!(baseline.ink_size(), (20, 10));
        let big = baseline.render(40, 20);
        assert_eq!(imaging::ink_bounds(&big), Some((225, 235, 40, 20)));
        assert_eq!(big.pixels().filter(|p| p.0[0] == 0).count(), 800);
    }

    #[test]
    fn scaling_again_from_the_baseline_adds_nothing_up() {
        let base = block_on(490, (100, 120, 31, 17));
        let baseline = Baseline::bitmap(&base).unwrap();
        let _ = baseline.render(90, 49);
        let _ = baseline.render(12, 7);
        let after_three = baseline.render(57, 31);
        let once = Baseline::bitmap(&base).unwrap().render(57, 31);
        assert_eq!(after_three, once);
    }

    #[test]
    fn huge_picture_crop_is_shrunk_to_the_area() {
        let mut rgb = RgbImage::from_pixel(2000, 1000, Rgb([255, 255, 255]));
        for y in 100..900 {
            for x in 100..1900 {
                rgb.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let baseline = Baseline::picture(&rgb, Mode::Bw, 490).unwrap();
        let Source::Picture { crop, .. } = &baseline.source else { panic!("not a picture") };
        assert!(crop.width() <= 490 && crop.height() <= 490, "{:?}", crop.dimensions());
        let (w, h) = baseline.ink_size();
        let (_, _, bw, bh) = imaging::ink_bounds(&baseline.render(w, h)).unwrap();
        assert!(bw.abs_diff(w) <= 1 && bh.abs_diff(h) <= 1, "{bw}x{bh} for {w}x{h}");
    }

    #[test]
    fn large_picture_crop_maps_back_to_the_original() {
        let mut rgb = RgbImage::from_pixel(980, 490, Rgb([255, 255, 255]));
        for y in 200..240 {
            for x in 400..480 {
                rgb.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        // Shrunk to 490x245 on the area: the 80x40 block becomes about 40x20.
        let baseline = Baseline::picture(&rgb, Mode::Bw, 490).unwrap();
        let (w, h) = baseline.ink_size();
        assert!((38..=42).contains(&w) && (18..=22).contains(&h), "{w}x{h}");
        // Twice the resolution of the original block, rebuilt from the original pixels.
        let (_, _, bw, bh) = imaging::ink_bounds(&baseline.render(160, 80)).unwrap();
        assert!((150..=160).contains(&bw) && (74..=80).contains(&bh), "{bw}x{bh}");
    }
}
