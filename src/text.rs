//! Text to bitmap, black on white, thresholded with Otsu like `CursorImage.ToBW()`.
//!
//! Glyphs are rasterised by `ab_glyph`; the Python prototype uses FreeType, so the two agree
//! on layout but not pixel for pixel.

use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont, point};
use image::{GrayImage, Luma};

use crate::imaging;

/// Extra pixels between lines and white border around the text, as in the prototype.
const LINE_GAP: f32 = 4.0;
const BORDER: i32 = 2;

/// Fonts tried when none is given, in order.
const DEFAULT_FONTS: [&str; 5] =
    ["DejaVuSans.ttf", "LiberationSans-Regular.ttf", "NotoSans-Regular.ttf", "arial.ttf", "Arial.ttf"];

#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error("font \"{0}\" not found: pass the path of a .ttf or .otf file")]
    FontNotFound(String),
    #[error("no usable font found on this system: pass the path of a .ttf or .otf file")]
    NoDefaultFont,
    #[error("cannot read font {path}: {reason}")]
    BadFont { path: String, reason: String },
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
        PathBuf::from("/Library/Fonts"),
        PathBuf::from("/System/Library/Fonts"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local/share/fonts"));
        dirs.push(Path::new(&home).join(".fonts"));
    }
    if let Some(windir) = std::env::var_os("WINDIR") {
        dirs.push(Path::new(&windir).join("Fonts"));
    }
    dirs
}

/// Looks for a file called `name` (case-insensitive) under `dir`, a few levels deep.
fn find_in(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let mut subdirs = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if entry.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            return Some(path);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs.iter().find_map(|d| find_in(d, name, depth - 1))
}

/// Path of the font: `name` as a file path, or as a file name in the system font folders;
/// without a name, the first of the default fonts that exists.
pub fn find_font(name: Option<&str>) -> Result<PathBuf, TextError> {
    let lookup = |name: &str| {
        let direct = Path::new(name);
        if direct.is_file() {
            return Some(direct.to_path_buf());
        }
        font_dirs().iter().find_map(|dir| find_in(dir, name, 4))
    };
    match name {
        Some(name) => lookup(name).ok_or_else(|| TextError::FontNotFound(name.to_owned())),
        None => DEFAULT_FONTS.iter().find_map(|name| lookup(name)).ok_or(TextError::NoDefaultFont),
    }
}

/// Renders `text` with an em size of `size` pixels. Lines are separated by a newline or by
/// the two characters `\n` (handy on the command line).
pub fn render(text: &str, font: Option<&str>, size: u32) -> Result<GrayImage, TextError> {
    let path = find_font(font)?;
    let bad_font = |reason: String| TextError::BadFont { path: path.display().to_string(), reason };
    let bytes = std::fs::read(&path).map_err(|e| bad_font(e.to_string()))?;
    let font = FontVec::try_from_vec(bytes).map_err(|e| bad_font(e.to_string()))?;
    // `PxScale` is the ascent-to-descent height; `size` is the em size, as in FreeType.
    let em = font.units_per_em().unwrap_or(1000.0);
    let scaled = font.as_scaled(PxScale::from(size as f32 * font.height_unscaled() / em));

    let text = text.replace("\\n", "\n");
    let line_height = scaled.ascent() + LINE_GAP;
    let mut outlines = Vec::new();
    for (row, line) in text.split('\n').enumerate() {
        let baseline = scaled.ascent() + row as f32 * line_height;
        let mut x = 0.0;
        let mut previous = None;
        for c in line.chars() {
            let id = scaled.glyph_id(c);
            if let Some(prev) = previous {
                x += scaled.kern(prev, id);
            }
            let glyph: Glyph = id.with_scale_and_position(scaled.scale(), point(x, baseline));
            x += scaled.h_advance(id);
            previous = Some(id);
            outlines.extend(scaled.outline_glyph(glyph));
        }
    }

    let bounds = outlines.iter().map(|o| o.px_bounds());
    let (min_x, min_y, max_x, max_y) = bounds.fold((i32::MAX, i32::MAX, i32::MIN, i32::MIN), |b, r| {
        (b.0.min(r.min.x as i32), b.1.min(r.min.y as i32), b.2.max(r.max.x as i32), b.3.max(r.max.y as i32))
    });
    if outlines.is_empty() {
        return Ok(GrayImage::from_pixel(1, 1, Luma([255])));
    }
    let width = (max_x - min_x + 2 * BORDER).max(1) as u32;
    let height = (max_y - min_y + 2 * BORDER).max(1) as u32;
    let mut im = GrayImage::from_pixel(width, height, Luma([255]));
    for outline in &outlines {
        let origin = outline.px_bounds().min;
        outline.draw(|x, y, coverage| {
            let px = origin.x as i32 + x as i32 - min_x + BORDER;
            let py = origin.y as i32 + y as i32 - min_y + BORDER;
            if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                let ink = 255 - (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
                let p = im.get_pixel_mut(px as u32, py as u32);
                p.0[0] = p.0[0].min(ink);
            }
        });
    }
    Ok(imaging::bw_otsu(&im))
}
