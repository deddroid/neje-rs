//! The image to engrave, built from the options shared by the CLI and the GUI in the order
//! the original program applies them: G-code, picture, rotations, reductions, text.

use std::path::PathBuf;

use image::GrayImage;

use crate::imaging::{self, ImageError, Mode};
use crate::text::{self, TextError};
use crate::{gcode, imaging::new_base};

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Text(#[from] TextError),
    #[error("cannot read G-code file {path}: {cause}")]
    Gcode { path: String, cause: std::io::Error },
}

/// Text to add on top of the image.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLayer {
    pub text: String,
    /// Font file or file name; `None` picks a default system font.
    pub font: Option<String>,
    /// Em size in pixels.
    pub size: u32,
    /// Centre of the text in pixels; `None` is the centre of the work area.
    pub at: Option<(i64, i64)>,
}

/// What to engrave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Job {
    pub image: Option<PathBuf>,
    pub mode: Mode,
    pub gcode: Option<PathBuf>,
    /// G-code: scale the drawing to fill the work area.
    pub gcode_fit: bool,
    /// G-code: line thickness in pixels.
    pub gcode_line_px: u32,
    /// Number of 90° clockwise rotations.
    pub rotate: u32,
    /// Number of reductions to 90 %.
    pub shrink: u32,
    pub text: Option<TextLayer>,
}

impl Job {
    /// True when the job has nothing to draw (the engraver keeps the image it already has).
    pub fn is_empty(&self) -> bool {
        self.image.is_none() && self.gcode.is_none() && self.text.is_none()
    }

    /// Builds the `base_size`² image (black = burn).
    pub fn build(&self, base_size: u32) -> Result<GrayImage, ComposeError> {
        let mut base = new_base(base_size);
        if let Some(path) = &self.gcode {
            let bytes =
                std::fs::read(path).map_err(|cause| ComposeError::Gcode { path: path.display().to_string(), cause })?;
            base = gcode::rasterize(
                &String::from_utf8_lossy(&bytes),
                base_size,
                self.gcode_fit,
                self.gcode_line_px.max(1),
            );
        }
        if let Some(path) = &self.image {
            let picture = imaging::prepare(&imaging::load(path)?, base_size, self.mode);
            base = imaging::center_on(&base, &picture);
        }
        for _ in 0..self.rotate {
            base = imaging::rotate90(&base);
        }
        for _ in 0..self.shrink {
            base = imaging::shrink(&base);
        }
        if let Some(layer) = &self.text {
            let stamp = text::render(&layer.text, layer.font.as_deref(), layer.size)?;
            let centre = i64::from(base_size / 2);
            let (cx, cy) = layer.at.unwrap_or((centre, centre));
            base = imaging::place(&base, &stamp, cx, cy);
        }
        Ok(base)
    }
}
