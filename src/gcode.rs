//! G-code to bitmap: lets the engraver (stock firmware, raster only) be used with programs
//! that export laser G-code (Inkscape, LaserWeb, ...).
//!
//! Supported: G0 G1 G2 G3 (I/J or R), G20/G21, G90/G91, G28, G92, M3/M4/M5, S, comments.
//! The laser burns on G1/G2/G3 moves while M3/M4 is active and S > 0 (when S is never
//! given, M3/M4 is enough).
//!
//! The arithmetic follows the Python prototype operation by operation, so both produce the
//! same pixels.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::hint::black_box;
use std::sync::LazyLock;

use image::GrayImage;
use regex::Regex;

use crate::draw;
use crate::imaging;

/// Pitch declared by NEJE: 490 px × 0.075 mm = 36.75 mm.
pub const MM_PER_PX: f64 = 0.075;

/// Empty border left around the drawing when it is scaled to fit.
const FIT_MARGIN_PX: f64 = 5.0;

/// A burnt segment in millimetres: (x0, y0, x1, y1).
pub type Segment = (f64, f64, f64, f64);

static WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z])[\s\x1f]*([-+]?[0-9]*\.?[0-9]+(?:[eE][-+]?[0-9]+)?)").expect("valid regex")
});
static PAREN_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(.*?\)").expect("valid regex"));

/// Line boundaries of Python's `str.splitlines`.
fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    // "\r\n" yields an empty line between the two characters, which is skipped anyway.
    text.split(is_line_break)
}

fn strip_comments(line: &str) -> String {
    let line = PAREN_COMMENT.replace_all(line, "");
    let code = line.split(';').next().unwrap_or("");
    code.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)).to_owned()
}

/// The burnt segments of a G-code program, in millimetres.
pub fn parse_segments(text: &str) -> Vec<Segment> {
    let (mut x, mut y) = (0.0f64, 0.0f64);
    let mut absolute = true;
    let mut scale = 1.0f64;
    let mut laser_on = false;
    let mut s_val: Option<f64> = None;
    let mut motion = 0i64;
    let mut segs = Vec::new();
    for raw in lines(text) {
        let line = strip_comments(raw);
        if line.is_empty() {
            continue;
        }
        let mut params: HashMap<char, f64> = HashMap::new();
        let mut gcodes = Vec::new();
        for cap in WORD.captures_iter(&line) {
            let letter = cap[1].chars().next().expect("one letter").to_ascii_uppercase();
            let Ok(value) = cap[2].parse::<f64>() else {
                continue;
            };
            match letter {
                'G' => gcodes.push(value),
                'M' => match value as i64 {
                    3 | 4 => laser_on = true,
                    5 | 2 | 30 => laser_on = false,
                    _ => {}
                },
                _ => {
                    params.insert(letter, value);
                }
            }
        }
        if let Some(&s) = params.get(&'S') {
            s_val = Some(s);
        }
        let mut new_motion = None;
        for g in gcodes {
            match (g * 10.0).round_ties_even() as i64 {
                200 => scale = 25.4,
                210 => scale = 1.0,
                900 => absolute = true,
                910 => absolute = false,
                280 => (x, y) = (0.0, 0.0),
                920 => {
                    // G92 redefines the current position (drawing stays in program coordinates).
                    if let Some(&px) = params.get(&'X') {
                        x = px * scale;
                    }
                    if let Some(&py) = params.get(&'Y') {
                        y = py * scale;
                    }
                    params.clear();
                }
                gi @ (0 | 10 | 20 | 30) => new_motion = Some(gi / 10),
                _ => {}
            }
        }
        if let Some(m) = new_motion {
            motion = m;
        }
        if !"XYIJR".chars().any(|k| params.contains_key(&k)) {
            continue;
        }
        let target = |current: f64, key: char| match params.get(&key) {
            Some(&v) if absolute => v * scale,
            Some(&v) => current + v * scale,
            None => current,
        };
        let (nx, ny) = (target(x, 'X'), target(y, 'Y'));
        let burning = laser_on && s_val.is_none_or(|s| s > 0.0) && (1..=3).contains(&motion);
        if motion == 2 || motion == 3 {
            let pts = arc(x, y, nx, ny, &params, scale, motion == 2);
            if burning {
                segs.extend(pts.windows(2).map(|p| (p[0].0, p[0].1, p[1].0, p[1].1)));
            }
        } else if burning {
            segs.push((x, y, nx, ny));
        }
        (x, y) = (nx, ny);
    }
    segs
}

/// Points along a G2 (clockwise) or G3 arc, about one every 0.05 mm.
fn arc(x0: f64, y0: f64, x1: f64, y1: f64, p: &HashMap<char, f64>, scale: f64, clockwise: bool) -> Vec<(f64, f64)> {
    let (cx, cy) = if let Some(&r) = p.get(&'R') {
        let r = r * scale;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let d = py_hypot(dx, dy);
        if d == 0.0 || r.abs() < d / 2.0 {
            return vec![(x0, y0), (x1, y1)];
        }
        // Python computes `(d / 2) ** 2` with the C `pow`; `black_box` keeps the compiler
        // from turning it into a multiplication, which can differ in the last bit.
        let h = (r * r - (d / 2.0).powf(black_box(2.0))).sqrt();
        let (mx, my) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let sgn = if clockwise ^ (r < 0.0) { -1.0 } else { 1.0 };
        (mx - sgn * h * dy / d, my + sgn * h * dx / d)
    } else {
        (x0 + p.get(&'I').copied().unwrap_or(0.0) * scale, y0 + p.get(&'J').copied().unwrap_or(0.0) * scale)
    };
    let r = py_hypot(x0 - cx, y0 - cy);
    let a0 = (y0 - cy).atan2(x0 - cx);
    let mut a1 = (y1 - cy).atan2(x1 - cx);
    if clockwise {
        while a1 >= a0 {
            a1 -= 2.0 * PI;
        }
    } else {
        while a1 <= a0 {
            a1 += 2.0 * PI;
        }
    }
    let n = (((a1 - a0).abs() * r.max(0.1) / 0.05) as i64).max(4);
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f64 / n as f64;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// `math.hypot(a, b)` as CPython computes it (`vector_norm` in `mathmodule.c`: scaled
/// squares summed in double-double arithmetic, plus one correction step). The C library
/// `hypot` can differ in the last bit, which is enough to move a pixel.
#[doc(hidden)]
pub fn py_hypot(a: f64, b: f64) -> f64 {
    let vec = [a.abs(), b.abs()];
    let max = if vec[1] > vec[0] { vec[1] } else { vec[0] };
    if vec[0].is_infinite() || vec[1].is_infinite() {
        return f64::INFINITY;
    }
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if max == 0.0 {
        return max;
    }
    let max_e = frexp_exponent(max);
    if max_e < -1023 {
        // ldexp(1.0, -max_e) would overflow: turn subnormals into normals first.
        return f64::MIN_POSITIVE * py_hypot(vec[0] / f64::MIN_POSITIVE, vec[1] / f64::MIN_POSITIVE);
    }
    let scale = pow2(-max_e);
    let mut sum = DoubleSum { csum: 1.0, frac1: 0.0, frac2: 0.0 };
    for v in vec {
        let x = v * scale;
        sum.add_product(x, x);
    }
    let mut h = sum.excess().sqrt();
    sum.add_product(-h, h);
    h += sum.excess() / (2.0 * h);
    h / scale
}

/// Running sum of products kept as a rounded total plus two error terms.
struct DoubleSum {
    csum: f64,
    frac1: f64,
    frac2: f64,
}

impl DoubleSum {
    /// Adds `x * y` without losing the rounding errors of the product and of the sum.
    fn add_product(&mut self, x: f64, y: f64) {
        let hi = x * y;
        let lo = x.mul_add(y, -hi);
        let total = self.csum + hi;
        let z = total - self.csum;
        self.frac1 += lo;
        self.frac2 += hi - z;
        self.csum = total;
    }

    /// The sum minus the initial 1.0.
    fn excess(&self) -> f64 {
        self.csum - 1.0 + (self.frac1 + self.frac2)
    }
}

/// Exponent `e` of C `frexp`: `x = m * 2^e` with 0.5 <= m < 1, for finite positive `x`.
fn frexp_exponent(x: f64) -> i32 {
    let biased = ((x.to_bits() >> 52) & 0x7ff) as i32;
    if biased == 0 {
        // Subnormal: normalise by 2^54 first.
        return frexp_exponent(x * pow2(54)) - 54;
    }
    biased - 1022
}

/// 2^n for -1074 <= n <= 1023.
fn pow2(n: i32) -> f64 {
    if n >= -1022 { f64::from_bits(((n + 1023) as u64) << 52) } else { f64::from_bits(1u64 << (n + 1074)) }
}

/// Draws the G-code on a `base_size`² bitmap (black = burn), with lines `line_px` thick.
///
/// Without `fit` the G-code origin (0,0) is the bottom left corner of the work area and
/// 1 px = 0.075 mm. With `fit` the drawing is scaled and centred in the area.
pub fn rasterize(text: &str, base_size: u32, fit: bool, line_px: u32) -> GrayImage {
    let segs = parse_segments(text);
    let mut im = imaging::new_base(base_size);
    if segs.is_empty() {
        return im;
    }
    let top = f64::from(base_size) - 1.0;
    let transform: Box<dyn Fn(f64, f64) -> (f64, f64)> = if fit {
        let bounds = |values: &mut dyn Iterator<Item = f64>| {
            values.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (if v < lo { v } else { lo }, if v > hi { v } else { hi })
            })
        };
        let (minx, maxx) = bounds(&mut segs.iter().flat_map(|s| [s.0, s.2]));
        let (miny, maxy) = bounds(&mut segs.iter().flat_map(|s| [s.1, s.3]));
        let (width, height) = (maxx - minx, maxy - miny);
        let span = [height, 1e-9].into_iter().fold(width, |m, v| if v > m { v } else { m });
        let room = top - 2.0 * FIT_MARGIN_PX;
        let k = room / span;
        let offx = FIT_MARGIN_PX + (room - width * k) / 2.0;
        let offy = FIT_MARGIN_PX + (room - height * k) / 2.0;
        Box::new(move |px, py| (offx + (px - minx) * k, top - (offy + (py - miny) * k)))
    } else {
        Box::new(move |px, py| (px / MM_PER_PX, top - py / MM_PER_PX))
    };
    for &(x0, y0, x1, y1) in &segs {
        draw::line(&mut im, transform(x0, y0), transform(x1, y1), 0, line_px);
    }
    im
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square() {
        let nc = "G21 G90\nG0 X0 Y0\nM3 S100\nG1 X10 Y0\nG1 X10 Y10\nM5\nG0 X20 Y20\n";
        assert_eq!(parse_segments(nc), [(0.0, 0.0, 10.0, 0.0), (10.0, 0.0, 10.0, 10.0)]);
        let im = rasterize(nc, 490, false, 1);
        assert_eq!(im.get_pixel((10.0 / 0.075) as u32, 489).0[0], 0);
    }

    #[test]
    fn relative_inch_arc() {
        let segs = parse_segments("G20 G91\nM3\nG1 X0.1\nG2 X0 Y0 I-0.05 J0\n");
        assert!((segs[0].2 - 2.54).abs() < 1e-9);
        assert!(segs.len() > 10);
    }

    #[test]
    fn comments_and_laser_off_moves_are_ignored() {
        let segs = parse_segments("(setup) G1 X5 ; no laser yet\nM3 S0\nG1 X9\nS10 G1 X12 (burn)\n");
        assert_eq!(segs, [(9.0, 0.0, 12.0, 0.0)]);
    }

    #[test]
    fn hypot_matches_exact_cases() {
        assert_eq!(py_hypot(3.0, 4.0), 5.0);
        assert_eq!(py_hypot(0.0, 0.0), 0.0);
        assert_eq!(py_hypot(-5.0, 0.0), 5.0);
        assert!(py_hypot(1e-320, 1e-320) > 0.0);
    }
}
