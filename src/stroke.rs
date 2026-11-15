//! Line art: shapes reduced to their centre line and redrawn with a stroke of a few pixels.
//!
//! On the engraver a line one pixel wide is a single continuous pass, and every extra
//! pixel of width is one more pass next to it (measured on a DK-8-KZ, 15/11/2026: an
//! 80 px tall letter takes 3.9 s with a 1 px stroke, 7.4 s with 2 px, 11 s with 3 px,
//! against tens of seconds for the same letter filled 10 px wide).

use image::{GrayImage, Luma};

/// Black-and-white picture as a grid of booleans (true = burn) with a white border, so
/// that neighbours never fall outside.
struct Grid {
    width: usize,
    height: usize,
    cells: Vec<bool>,
}

impl Grid {
    fn from_image(image: &GrayImage) -> Self {
        let (width, height) = (image.width() as usize + 2, image.height() as usize + 2);
        let mut cells = vec![false; width * height];
        for (x, y, p) in image.enumerate_pixels() {
            cells[(y as usize + 1) * width + x as usize + 1] = p.0[0] != 255;
        }
        Grid { width, height, cells }
    }

    fn to_image(&self) -> GrayImage {
        GrayImage::from_fn(self.width as u32 - 2, self.height as u32 - 2, |x, y| {
            Luma([if self.cells[(y as usize + 1) * self.width + x as usize + 1] { 0 } else { 255 }])
        })
    }

    /// The eight neighbours of cell `i`, clockwise from north (P2..P9 of Zhang and Suen).
    fn neighbours(&self, i: usize) -> [bool; 8] {
        let w = self.width;
        [i - w, i - w + 1, i + 1, i + w + 1, i + w, i + w - 1, i - 1, i - w - 1].map(|n| self.cells[n])
    }

    /// Indices of the cells that are not on the border.
    fn inner(&self) -> impl Iterator<Item = usize> + use<> {
        let (w, h) = (self.width, self.height);
        (1..h - 1).flat_map(move |y| (1..w - 1).map(move |x| y * w + x))
    }
}

/// Zhang–Suen thinning: peels the outline of every shape, alternating two passes, until
/// only lines one pixel wide are left.
fn thin(grid: &mut Grid) {
    // Only the cells still black can change: the list shrinks as the shapes get thinner.
    let mut alive: Vec<usize> = grid.inner().filter(|&i| grid.cells[i]).collect();
    loop {
        let mut changed = false;
        for second_pass in [false, true] {
            let doomed: Vec<usize> = alive
                .iter()
                .copied()
                .filter(|&i| {
                    let p = grid.neighbours(i);
                    let black = p.iter().filter(|&&b| b).count();
                    let transitions = (0..8).filter(|&k| !p[k] && p[(k + 1) % 8]).count();
                    let (north, east, south, west) = (p[0], p[2], p[4], p[6]);
                    let side = if second_pass {
                        !(north && east && west) && !(north && south && west)
                    } else {
                        !(north && east && south) && !(east && south && west)
                    };
                    (2..=6).contains(&black) && transitions == 1 && side
                })
                .collect();
            changed |= !doomed.is_empty();
            for i in doomed {
                grid.cells[i] = false;
            }
            alive.retain(|&i| grid.cells[i]);
        }
        if !changed {
            return;
        }
    }
}

/// Thinning erases some tiny shapes completely (a 2×2 square, for one): put back, as they
/// were, the shapes of `original` that left nothing in `thinned`.
fn restore_vanished(original: &Grid, thinned: &mut Grid) {
    let mut seen = vec![false; original.cells.len()];
    let w = original.width;
    for start in original.inner() {
        if !original.cells[start] || seen[start] {
            continue;
        }
        // Flood fill of one 8-connected shape.
        let mut shape = vec![start];
        seen[start] = true;
        let mut next = 0;
        while let Some(&i) = shape.get(next) {
            next += 1;
            for n in [i - w - 1, i - w, i - w + 1, i - 1, i + 1, i + w - 1, i + w, i + w + 1] {
                if original.cells[n] && !seen[n] {
                    seen[n] = true;
                    shape.push(n);
                }
            }
        }
        if !shape.iter().any(|&i| thinned.cells[i]) {
            for i in shape {
                thinned.cells[i] = true;
            }
        }
    }
}

/// Offsets of a round brush `stroke` pixels wide (1 = the pixel itself, 2 = a 2×2 block).
fn brush(stroke: u8) -> Vec<(i64, i64)> {
    let n = i64::from(stroke.max(1));
    let centre = (n - 1) as f64 / 2.0;
    let radius2 = (n as f64 / 2.0).powi(2);
    let anchor = (n - 1) / 2;
    (0..n)
        .flat_map(|dy| (0..n).map(move |dx| (dx, dy)))
        .filter(|&(dx, dy)| (dx as f64 - centre).powi(2) + (dy as f64 - centre).powi(2) <= radius2)
        .map(|(dx, dy)| (dx - anchor, dy - anchor))
        .collect()
}

/// The centre lines of the shapes of `image` (anything not pure white), one pixel wide.
pub fn centre_lines(image: &GrayImage) -> GrayImage {
    let original = Grid::from_image(image);
    let mut thinned = Grid { width: original.width, height: original.height, cells: original.cells.clone() };
    thin(&mut thinned);
    restore_vanished(&original, &mut thinned);
    thinned.to_image()
}

/// `image` as line art: its centre lines drawn `stroke` pixels wide.
pub fn line_art(image: &GrayImage, stroke: u8) -> GrayImage {
    let lines = centre_lines(image);
    if stroke <= 1 {
        return lines;
    }
    let brush = brush(stroke);
    let mut out = GrayImage::from_pixel(image.width(), image.height(), Luma([255]));
    for (x, y, p) in lines.enumerate_pixels() {
        if p.0[0] != 0 {
            continue;
        }
        for &(dx, dy) in &brush {
            let (px, py) = (i64::from(x) + dx, i64::from(y) + dy);
            if (0..i64::from(out.width())).contains(&px) && (0..i64::from(out.height())).contains(&py) {
                out.put_pixel(px as u32, py as u32, Luma([0]));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn black(im: &GrayImage) -> usize {
        im.pixels().filter(|p| p.0[0] == 0).count()
    }

    fn bar() -> GrayImage {
        GrayImage::from_fn(60, 100, |x, y| Luma([if (20..30).contains(&x) && (10..90).contains(&y) { 0 } else { 255 }]))
    }

    #[test]
    fn a_thick_bar_becomes_one_pixel_wide() {
        let lines = centre_lines(&bar());
        for y in 20..80 {
            let row: Vec<u32> = (0..60).filter(|&x| lines.get_pixel(x, y).0[0] == 0).collect();
            assert_eq!(row.len(), 1, "row {y}: {row:?}");
            assert!((23..=26).contains(&row[0]), "the line is not in the middle: {row:?}");
        }
    }

    #[test]
    fn stroke_width_is_respected() {
        let one = line_art(&bar(), 1);
        for stroke in 2..=4u32 {
            let wide = line_art(&bar(), stroke as u8);
            let row: Vec<u32> = (0..60).filter(|&x| wide.get_pixel(x, 50).0[0] == 0).collect();
            assert_eq!(row.len() as u32, stroke);
            assert!(black(&wide) > black(&one));
        }
    }

    #[test]
    fn thin_lines_are_left_alone() {
        let line = GrayImage::from_fn(40, 40, |x, y| Luma([if x == y || y == 5 { 0 } else { 255 }]));
        assert_eq!(centre_lines(&line), line);
    }

    #[test]
    fn tiny_shapes_do_not_vanish() {
        let dot =
            GrayImage::from_fn(10, 10, |x, y| Luma([if (4..6).contains(&x) && (4..6).contains(&y) { 0 } else { 255 }]));
        assert!(black(&centre_lines(&dot)) > 0);
        let single = GrayImage::from_fn(5, 5, |x, y| Luma([if (x, y) == (0, 0) { 0 } else { 255 }]));
        assert_eq!(centre_lines(&single), single);
    }

    #[test]
    fn shapes_stay_connected() {
        // An L: the centre line must still join the two arms.
        let l = GrayImage::from_fn(60, 60, |x, y| {
            Luma([
                if ((10..18).contains(&x) && (10..50).contains(&y)) || ((10..50).contains(&x) && (42..50).contains(&y))
                {
                    0
                } else {
                    255
                },
            ])
        });
        let lines = centre_lines(&l);
        let grid = Grid::from_image(&lines);
        let start = grid.inner().find(|&i| grid.cells[i]).unwrap();
        let mut seen = vec![false; grid.cells.len()];
        let mut stack = vec![start];
        seen[start] = true;
        let mut reached = 0;
        while let Some(i) = stack.pop() {
            reached += 1;
            let w = grid.width;
            for n in [i - w - 1, i - w, i - w + 1, i - 1, i + 1, i + w - 1, i + w, i + w + 1] {
                if grid.cells[n] && !seen[n] {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
        assert_eq!(reached, black(&lines));
    }
}
