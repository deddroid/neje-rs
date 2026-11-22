//! Free software for the NEJE DK-8-KZ laser engraver and its relatives.
//!
//! The library is split in a pure part, testable without the machine, and the serial link:
//! - [`protocol`]: commands, incoming messages and the image payload of protocols
//!   V10/V20/V21/V30;
//! - [`imaging`]: shrink to the work area, grey, Otsu, NEJE dithering, canvas operations;
//! - [`scale`]: scaling the content of the work area to a size;
//! - [`gcode`]: G-code to bitmap;
//! - [`text`]: text to bitmap;
//! - [`plan`]: print modes, job splitting and time estimates; [`stroke`]: line art;
//! - [`compose`]: the image to engrave from the options shared by the CLI and the GUI;
//! - [`device`]: serial port detection and the engraver session;
//! - [`doctor`]: what the system lacks for the engraver (driver, permissions) and its fix;
//! - [`simulator`]: a virtual engraver on a pseudo-terminal (Unix).

pub mod bitmap;
pub mod compose;
pub mod device;
pub mod doctor;
mod draw;
pub mod gcode;
pub mod i18n;
pub mod imaging;
pub mod messages;
pub mod plan;
pub mod protocol;
mod resample;
pub mod scale;
#[cfg(unix)]
pub mod simulator;
pub mod stroke;
pub mod text;
