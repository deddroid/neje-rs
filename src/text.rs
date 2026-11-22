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

/// Font used when none is given: Inter, built into the program, so that text works on
/// every system without looking for installed fonts.
const BUILT_IN_FONT: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");

/// Largest em size accepted by [`size_for_height`].
pub const MAX_SIZE: u32 = 1000;

/// Largest area of a rendered text, in pixels.
const MAX_AREA: i64 = 64_000_000;

/// Where the font of a text comes from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FontSource {
    /// Inter, built into the program.
    #[default]
    BuiltIn,
    /// Face `index` of a font file (0 unless the file is a collection).
    File { path: PathBuf, index: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error("font \"{0}\" not found: pass the path of a .ttf or .otf file")]
    FontNotFound(String),
    #[error("cannot read font {path}: {reason}")]
    BadFont { path: String, reason: String },
}

pub(crate) fn font_dirs() -> Vec<PathBuf> {
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

/// Path of the font called `name`: a file path, or a file name in the system font folders.
pub fn find_font(name: &str) -> Result<PathBuf, TextError> {
    let direct = Path::new(name);
    if direct.is_file() {
        return Ok(direct.to_path_buf());
    }
    font_dirs().iter().find_map(|dir| find_in(dir, name, 4)).ok_or_else(|| TextError::FontNotFound(name.to_owned()))
}

/// Loads the font of `source`.
pub fn load(source: &FontSource) -> Result<FontVec, TextError> {
    match source {
        FontSource::BuiltIn => FontVec::try_from_vec(BUILT_IN_FONT.to_vec())
            .map_err(|e| TextError::BadFont { path: "built-in".to_owned(), reason: e.to_string() }),
        FontSource::File { path, index } => {
            let origin = path.display().to_string();
            let bytes =
                std::fs::read(path).map_err(|e| TextError::BadFont { path: origin.clone(), reason: e.to_string() })?;
            FontVec::try_from_vec_and_index(bytes, *index)
                .map_err(|e| TextError::BadFont { path: origin, reason: e.to_string() })
        }
    }
}

/// The font to draw with: the file called `name`, or the built-in one.
fn load_font(name: Option<&str>) -> Result<FontVec, TextError> {
    let source = match name {
        Some(name) => FontSource::File { path: find_font(name)?, index: 0 },
        None => FontSource::BuiltIn,
    };
    load(&source)
}

/// Renders `text` with an em size of `size` pixels, in the font called `font` (see
/// [`find_font`]) or in the built-in Inter. Lines are separated by a newline or by
/// the two characters `\n` (handy on the command line).
pub fn render(text: &str, font: Option<&str>, size: u32) -> Result<GrayImage, TextError> {
    Ok(render_with(&load_font(font)?, text, size))
}

/// Like [`render`], with a font already loaded by [`load`].
pub fn render_with(font: &FontVec, text: &str, size: u32) -> GrayImage {
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
        return GrayImage::from_pixel(1, 1, Luma([255]));
    }
    // A malformed font can report absurd glyph bounds: refuse a canvas that would not fit in
    // memory (or overflow) rather than try to allocate it.
    let border = 2 * i64::from(BORDER);
    let width = (i64::from(max_x) - i64::from(min_x) + border).max(1);
    let height = (i64::from(max_y) - i64::from(min_y) + border).max(1);
    if width * height > MAX_AREA {
        return GrayImage::from_pixel(1, 1, Luma([255]));
    }
    let (width, height) = (width as u32, height as u32);
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
    imaging::bw_otsu(&im)
}

/// Em size at which [`render_with`] draws the ink of `text` about `height` pixels tall (within
/// a pixel or two), without the white border around it: a first guess, then one proportional
/// correction. `text` must contain at least one visible glyph.
pub fn size_for_height(font: &FontVec, text: &str, height: u32) -> u32 {
    // Measured on the ink after the threshold, as the Size row of the window measures it.
    let inner = |size: u32| f64::from(imaging::ink_bounds(&render_with(font, text, size)).map_or(1, |(_, _, _, h)| h));
    let target = f64::from(height.max(1));
    let guess = 100;
    let first = (f64::from(guess) * target / inner(guess)).round().clamp(1.0, f64::from(MAX_SIZE)) as u32;
    (f64::from(first) * target / inner(first)).round().clamp(1.0, f64::from(MAX_SIZE)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn black(im: &GrayImage) -> usize {
        im.pixels().filter(|p| p.0[0] == 0).count()
    }

    #[test]
    fn built_in_font_draws_text() {
        let one = render("NEJE", None, 48).unwrap();
        assert!(one.width() > 80 && one.height() > 30, "{:?}", one.dimensions());
        assert!(black(&one) > 500);
        let two = render("NEJE\\nDK-8", None, 48).unwrap();
        assert!(two.height() > one.height() + 30, "the second line is missing");
    }

    #[test]
    fn empty_text_is_a_white_pixel() {
        assert_eq!(black(&render("", None, 48).unwrap()), 0);
    }

    #[test]
    fn missing_font_is_reported() {
        assert!(matches!(render("x", Some("no-such-font-here.ttf"), 20), Err(TextError::FontNotFound(_))));
    }

    #[test]
    fn symbols_of_the_window_are_in_the_built_in_font() {
        // The window draws its text with the same font; the rest comes from egui's own.
        let font = ab_glyph::FontRef::try_from_slice(BUILT_IN_FONT).unwrap();
        for c in "…×–•✓".chars() {
            assert_ne!(font.glyph_id(c).0, 0, "Inter lacks {c:?}");
        }
    }

    #[test]
    fn render_with_matches_render() {
        let font = load(&FontSource::BuiltIn).unwrap();
        assert_eq!(render_with(&font, "NEJE", 48), render("NEJE", None, 48).unwrap());
    }

    #[test]
    fn font_file_by_path_and_index() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/Inter-Regular.ttf");
        let font = load(&FontSource::File { path, index: 0 }).unwrap();
        assert_eq!(render_with(&font, "Hi", 40), render("Hi", None, 40).unwrap());
        let missing = FontSource::File { path: PathBuf::from("/no/such/font.ttf"), index: 0 };
        assert!(matches!(load(&missing), Err(TextError::BadFont { .. })));
    }

    #[test]
    fn size_for_height_hits_the_height() {
        let font = load(&FontSource::BuiltIn).unwrap();
        for target in [30u32, 80, 200] {
            let size = size_for_height(&font, "Hello\\nWorld", target);
            let (_, _, _, got) = imaging::ink_bounds(&render_with(&font, "Hello\\nWorld", size)).unwrap();
            assert!(got.abs_diff(target) <= 1, "asked {target}, got {got} at size {size}");
        }
    }
}
