//! Line drawing identical to Pillow's `ImageDraw.line` on an 8-bit image
//! (`libImaging/Draw.c`): Bresenham for width 1, a filled quadrilateral for wider lines.
//! The G-code rasteriser of the Python prototype draws with Pillow, so this port keeps the
//! two outputs equal pixel for pixel.

use image::GrayImage;

fn point(im: &mut GrayImage, x: i32, y: i32, ink: u8) {
    if x >= 0 && (x as u32) < im.width() && y >= 0 && (y as u32) < im.height() {
        im.put_pixel(x as u32, y as u32, image::Luma([ink]));
    }
}

/// `hline8`: horizontal run from `x0` to `x1` inclusive, clipped to the image.
fn hline(im: &mut GrayImage, x0: i32, y: i32, x1: i32, ink: u8) {
    let (w, h) = (im.width() as i32, im.height() as i32);
    if y < 0 || y >= h || x0 >= w || x1 < 0 {
        return;
    }
    for x in x0.max(0)..=x1.min(w - 1) {
        im.put_pixel(x as u32, y as u32, image::Luma([ink]));
    }
}

/// `line`: one-pixel line from (`x0`, `y0`) to (`x1`, `y1`), end point excluded.
fn thin_line(im: &mut GrayImage, mut x0: i32, mut y0: i32, x1: i32, y1: i32, ink: u8) {
    let mut dy = y1 - y0;
    let ys = if dy < 0 { -1 } else { 1 };
    dy = dy.abs();
    if dy == 0 {
        if x1 > x0 {
            hline(im, x0, y0, x1 - 1, ink);
        } else if x0 > x1 {
            hline(im, x1 + 1, y0, x0, ink);
        }
        return;
    }
    let mut dx = x1 - x0;
    let xs = if dx < 0 { -1 } else { 1 };
    dx = dx.abs();
    if dx == 0 {
        for _ in 0..dy {
            point(im, x0, y0, ink);
            y0 += ys;
        }
    } else if dx > dy {
        let n = dx;
        dy += dy;
        let mut e = dy - dx;
        dx += dx;
        for _ in 0..n {
            point(im, x0, y0, ink);
            if e >= 0 {
                y0 += ys;
                e -= dx;
            }
            e += dy;
            x0 += xs;
        }
    } else {
        let n = dy;
        dx += dx;
        let mut e = dx - dy;
        dy += dy;
        for _ in 0..n {
            point(im, x0, y0, ink);
            if e >= 0 {
                x0 += xs;
                e -= dy;
            }
            e += dx;
            y0 += ys;
        }
    }
}

/// Pillow's `ROUND_UP` / `ROUND_DOWN`: round half away from / towards zero.
fn round_up(f: f64) -> i32 {
    if f >= 0.0 { (f + 0.5).floor() as i32 } else { -((f.abs() + 0.5).floor()) as i32 }
}

fn round_down(f: f64) -> i32 {
    if f >= 0.0 { (f - 0.5).ceil() as i32 } else { -((f.abs() - 0.5).ceil()) as i32 }
}

/// The same macros applied to a C `float`: the ±0.5 is added in single precision.
fn round_up_f32(f: f32) -> i32 {
    if f >= 0.0 { (f + 0.5).floor() as i32 } else { -((f.abs() + 0.5).floor()) as i32 }
}

fn round_down_f32(f: f32) -> i32 {
    if f >= 0.0 { (f - 0.5).ceil() as i32 } else { -((f.abs() - 0.5).ceil()) as i32 }
}

struct Edge {
    x0: i32,
    y0: i32,
    xmin: i32,
    ymin: i32,
    xmax: i32,
    ymax: i32,
    dx: f32,
}

impl Edge {
    fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        let dx = if y0 == y1 { 0.0 } else { (x1 - x0) as f32 / (y1 - y0) as f32 };
        Edge { x0, y0, xmin: x0.min(x1), ymin: y0.min(y1), xmax: x0.max(x1), ymax: y0.max(y1), dx }
    }

    /// X of the edge on scan line `y`, in single precision like the C code.
    fn x_at(&self, y: i32) -> f32 {
        (y - self.y0) as f32 * self.dx + self.x0 as f32
    }
}

/// `polygon_generic` without alpha blending: scan-line fill of a closed polygon.
fn polygon(im: &mut GrayImage, edges: &[Edge], ink: u8) {
    let mut ymin = im.height() as i32 - 1;
    let mut ymax = 0;
    let mut table = Vec::with_capacity(edges.len());
    for e in edges {
        ymin = ymin.min(e.ymin);
        ymax = ymax.max(e.ymax);
        if e.ymin == e.ymax {
            hline(im, e.xmin, e.ymin, e.xmax, ink);
        } else {
            table.push(e);
        }
    }
    ymin = ymin.max(0);
    ymax = ymax.min(im.height() as i32);
    let mut xx: Vec<f32> = Vec::with_capacity(table.len() * 2);
    for y in ymin..=ymax {
        xx.clear();
        for (i, current) in table.iter().enumerate() {
            if y < current.ymin || y > current.ymax {
                continue;
            }
            xx.push(current.x_at(y));
            if y == current.ymax && y < ymax {
                // Needed to draw consistent polygons.
                xx.push(current.x_at(y));
            } else if (y == current.ymin || y == current.ymax) && current.dx != 0.0 {
                // Connect discontiguous corners.
                let here = *xx.last().expect("just pushed");
                for other in &table[..i] {
                    if (y != other.ymin && y != other.ymax) || other.dx == 0.0 {
                        continue;
                    }
                    if here.round() != other.x_at(y).round() {
                        continue;
                    }
                    // Look at the next row, or the previous one on the last row.
                    let adjacent_y = y + if y == current.ymax { -1 } else { 1 };
                    let adjacent = current.x_at(adjacent_y);
                    if adjacent_y >= other.ymin && adjacent_y <= other.ymax {
                        let adjacent_other = other.x_at(adjacent_y);
                        let last = xx.last_mut().expect("just pushed");
                        if here > adjacent + 1.0 && here > adjacent_other + 1.0 {
                            *last = (f64::from(adjacent).max(f64::from(adjacent_other)) as f32).round() + 1.0;
                        } else if here < adjacent - 1.0 && here < adjacent_other - 1.0 {
                            *last = (f64::from(adjacent).min(f64::from(adjacent_other)) as f32).round() - 1.0;
                        }
                        break;
                    }
                }
            }
        }
        xx.sort_by(f32::total_cmp);
        for pair in xx.as_chunks::<2>().0 {
            hline(im, round_up_f32(pair[0]), y, round_down_f32(pair[1]), ink);
        }
    }
}

/// `ImagingDrawWideLine`: the line as a quadrilateral `width` pixels thick.
fn wide_line(im: &mut GrayImage, x0: i32, y0: i32, x1: i32, y1: i32, ink: u8, width: u32) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx == 0 && dy == 0 {
        point(im, x0, y0, ink);
        return;
    }
    let big = f64::from(dx).hypot(f64::from(dy));
    let small = (f64::from(width) - 1.0) / 2.0;
    let ratio_max = f64::from(round_up(small)) / big;
    let ratio_min = f64::from(round_down(small)) / big;
    let dxmin = round_down(ratio_min * f64::from(dy));
    let dxmax = round_down(ratio_max * f64::from(dy));
    let dymin = round_down(ratio_min * f64::from(dx));
    let dymax = round_down(ratio_max * f64::from(dx));
    let v = [(x0 - dxmin, y0 + dymax), (x1 - dxmin, y1 + dymax), (x1 + dxmax, y1 - dymin), (x0 + dxmax, y0 - dymin)];
    let edges: Vec<Edge> = (0..4).map(|i| Edge::new(v[i].0, v[i].1, v[(i + 1) % 4].0, v[(i + 1) % 4].1)).collect();
    polygon(im, &edges, ink);
}

/// `ImageDraw.line([(x0, y0), (x1, y1)], fill=ink, width=width)`: coordinates are truncated
/// to integers; a one-pixel line also gets its end point.
pub fn line(im: &mut GrayImage, from: (f64, f64), to: (f64, f64), ink: u8, width: u32) {
    let (x0, y0, x1, y1) = (from.0 as i32, from.1 as i32, to.0 as i32, to.1 as i32);
    match width {
        0 => {}
        1 => {
            thin_line(im, x0, y0, x1, y1, ink);
            point(im, x1, y1, ink);
        }
        _ => wide_line(im, x0, y0, x1, y1, ink, width),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn black(im: &GrayImage) -> Vec<(u32, u32)> {
        im.enumerate_pixels().filter(|(_, _, p)| p.0[0] == 0).map(|(x, y, _)| (x, y)).collect()
    }

    #[test]
    fn thin_horizontal_line_includes_both_ends() {
        let mut im = GrayImage::from_pixel(8, 3, image::Luma([255]));
        line(&mut im, (1.9, 1.2), (5.0, 1.0), 0, 1);
        assert_eq!(black(&im), [(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
    }

    #[test]
    fn lines_are_clipped() {
        let mut im = GrayImage::from_pixel(4, 4, image::Luma([255]));
        line(&mut im, (-5.0, -5.0), (10.0, 10.0), 0, 1);
        assert_eq!(black(&im), [(0, 0), (1, 1), (2, 2), (3, 3)]);
        line(&mut im, (-5.0, 2.0), (10.0, 2.0), 0, 3);
        assert_eq!(black(&im).len(), 4 + 3 * 4 - 3);
    }
}
