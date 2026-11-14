//! Free software for the NEJE DK-8-KZ laser engraver and its relatives.
//!
//! The library is split in a pure part, testable without the machine, and the serial link:
//! - [`protocol`]: commands, incoming messages and the image payload of protocols
//!   V10/V20/V21/V30;
//! - [`imaging`]: shrink to the work area, grey, Otsu, NEJE dithering, canvas operations;
//! - [`gcode`]: G-code to bitmap;
//! - [`text`]: text to bitmap;
//! - [`compose`]: the image to engrave from the options shared by the CLI and the GUI;
//! - [`device`]: serial port detection and the engraver session;
//! - [`simulator`]: a virtual engraver on a pseudo-terminal (Unix).

pub mod bitmap;
pub mod compose;
pub mod device;
mod draw;
pub mod gcode;
pub mod i18n;
pub mod imaging;
pub mod messages;
pub mod protocol;
mod resample;
#[cfg(unix)]
pub mod simulator;
pub mod text;
