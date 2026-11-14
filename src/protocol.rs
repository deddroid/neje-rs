//! Serial protocols of the NEJE DK-8-KZ family, rebuilt from the official NEJE V3.6 software.
//!
//! The launcher (`NEJE V3.6.exe`) embeds four programs, one per firmware generation:
//! `V10` (single-byte commands, the DK-8-KZ "NEJE-DK3000") and `V20`/`V21`/`V30` (4-byte
//! frames starting with `0xFF`). Every value here comes from the decompiled code; the method
//! names in the comments are those of the original (`LaserCarver.FormOld`, `Instruction`,
//! `FormMain`, `FormWelcome`). All versions use 57600 baud, 8N1, no flow control.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use image::GrayImage;

use crate::bitmap;
use crate::imaging;

pub const BAUD_RATE: u32 = 57_600;

/// Burn time accepted by every firmware (`trackBar_hottime`: 1–120, default 30).
pub const BURN_MIN: u8 = 1;
pub const BURN_MAX: u8 = 120;
pub const BURN_DEFAULT: u8 = 30;

/// Size of the image sent to the V10 firmware: a complete 1-bit BMP of 512×512 pixels.
pub const V10_PAYLOAD_LEN: usize = 32_830;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("command \"{command}\" is not available in protocol {version}")]
    Unsupported { command: &'static str, version: Version },
    #[error("burn time out of range {BURN_MIN}..{BURN_MAX}: {0}")]
    BurnOutOfRange(u8),
    #[error("coordinate out of range 0..25599: {0}")]
    CoordinateOutOfRange(u16),
    #[error("unknown protocol \"{0}\" (expected V10, V20, V21 or V30)")]
    UnknownVersion(String),
}

/// Firmware generation, named after the program the original launcher starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Version {
    V10,
    V20,
    V21,
    V30,
}

impl Version {
    /// Same order as `FormWelcome.ConnectInstruction` in the original launcher.
    pub const DETECTION_ORDER: [Version; 4] = [Version::V10, Version::V20, Version::V21, Version::V30];

    pub fn name(self) -> &'static str {
        match self {
            Version::V10 => "V10",
            Version::V20 => "V20",
            Version::V21 => "V21",
            Version::V30 => "V30",
        }
    }

    /// Bytes that ask the engraver to identify itself (`timer_Serial_Tick` / `ConnectAsk`).
    pub fn handshake(self) -> &'static [u8] {
        match self {
            Version::V10 => &[0xF6],
            _ => &[0xFF, 0x09, 0x00, 0x00],
        }
    }

    /// Recognises the reply to [`Version::handshake`] the way the launcher does
    /// ("the reply contains..."). Returns the protocol, with the hardware version for V30.
    pub fn recognise(self, reply: &[u8]) -> Option<Protocol> {
        let contains = |pattern: &[u8]| reply.windows(pattern.len()).any(|w| w == pattern);
        match self {
            Version::V10 => contains(&[0x65]).then(|| Protocol::new(self, None)),
            Version::V20 => contains(&[0xFF, 0x01, 0x00, 0x00]).then(|| Protocol::new(self, None)),
            Version::V21 => contains(&[0xFF, 0x02, 0x64]).then(|| Protocol::new(self, None)),
            Version::V30 => [11u8, 12, 13]
                .into_iter()
                .find(|&hw| contains(&[0xFF, 0x02, hw]))
                .map(|hw| Protocol::new(self, Some(hw))),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Version {
    type Err = ProtocolError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Version::DETECTION_ORDER
            .into_iter()
            .find(|v| v.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| ProtocolError::UnknownVersion(s.to_owned()))
    }
}

/// Head movement direction (same numbering in every protocol).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up = 1,
    Down = 2,
    Left = 3,
    Right = 4,
}

/// A command for the engraver. [`Protocol::encode`] turns it into bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Start engraving the image in memory. V2x/V30 ignore `burn` (send `SetBurn` first).
    Start {
        burn: u8,
    },
    SetBurn(u8),
    Pause,
    /// "Reboot" in the original program.
    Reset,
    /// "To Origin" (V10 only).
    Home,
    Center,
    /// "Box Locating": traces the perimeter of the image.
    Box,
    Move(Direction),
    Forward,
    Backward,
    Redo,
    /// Erase the image memory and prepare to receive a new image.
    Erase,
    MoveTo {
        x: u16,
        y: u16,
    },
    PowerOn,
    PowerOff,
}

impl Command {
    /// Name used in messages and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Command::Start { .. } => "start",
            Command::SetBurn(_) => "set-burn",
            Command::Pause => "pause",
            Command::Reset => "reset",
            Command::Home => "home",
            Command::Center => "center",
            Command::Box => "box",
            Command::Move(_) => "move",
            Command::Forward => "forward",
            Command::Backward => "backward",
            Command::Redo => "redo",
            Command::Erase => "erase",
            Command::MoveTo { .. } => "move-to",
            Command::PowerOn => "power-on",
            Command::PowerOff => "power-off",
        }
    }

    /// True for commands that can switch the laser on: engraving (`F1`), the box preview
    /// (`F4`) and the progress navigation (`FC xx`). Callers ask for confirmation first.
    pub fn fires_laser(self) -> bool {
        matches!(self, Command::Start { .. } | Command::Box | Command::Forward | Command::Backward | Command::Redo)
    }
}

/// A firmware generation plus, for V30, the hardware version reported at connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protocol {
    pub version: Version,
    pub hw: Option<u8>,
}

impl Protocol {
    pub fn new(version: Version, hw: Option<u8>) -> Self {
        Protocol { version, hw }
    }

    pub fn handshake(self) -> &'static [u8] {
        self.version.handshake()
    }

    /// Side of the square work area in pixels (`BaseImage`): 490 px × 0.075 mm = 36.75 mm.
    pub fn base_size(self) -> u32 {
        match self.version {
            Version::V10 | Version::V21 => 490,
            Version::V20 => 550,
            // FormMain: 550 when HardVersion == 12, otherwise 490.
            Version::V30 => {
                if self.hw == Some(12) {
                    550
                } else {
                    490
                }
            }
        }
    }

    /// Size of the canvas the image is placed on before sending (width, height).
    pub fn upload_size(self) -> (u32, u32) {
        match self.version {
            Version::V10 | Version::V21 => (512, 512),
            Version::V20 => (576, 552),
            Version::V30 => {
                if self.hw == Some(102) {
                    (552, 572)
                } else {
                    (512, 512)
                }
            }
        }
    }

    /// Number of image bytes sent after the erase command.
    pub fn payload_len(self) -> usize {
        match self.version {
            Version::V10 => V10_PAYLOAD_LEN,
            _ => {
                let (w, h) = self.upload_size();
                bitmap::stride_1bpp(w) * h as usize
            }
        }
    }

    /// True when the firmware confirms the erase (`EraseDone`); V10 only gets a fixed wait.
    pub fn confirms_erase(self) -> bool {
        self.version != Version::V10
    }

    /// V10: how long to wait after the erase command (the original sleeps 3 s and then
    /// spends ~0.2–0.3 s converting the image; 3.5 s leaves a margin).
    /// Other versions: how long to wait for `EraseDone` before giving up.
    pub fn erase_wait(self) -> Duration {
        match self.version {
            Version::V10 => Duration::from_millis(3500),
            Version::V20 | Version::V21 => Duration::from_secs(5),
            Version::V30 => Duration::from_millis(2500),
        }
    }

    /// True when the firmware reports the engraving position (`FF xh xl yh yl` on V10).
    pub fn reports_progress(self) -> bool {
        self.version != Version::V30
    }

    /// Bytes for `command`, or an error when this protocol has no such command or a value
    /// is out of range.
    pub fn encode(self, command: Command) -> Result<Vec<u8>, ProtocolError> {
        let unsupported = || ProtocolError::Unsupported { command: command.name(), version: self.version };
        let check_burn = |burn: u8| {
            if (BURN_MIN..=BURN_MAX).contains(&burn) { Ok(burn) } else { Err(ProtocolError::BurnOutOfRange(burn)) }
        };
        if self.version == Version::V10 {
            // LaserCarver.FormOld (button_*_Click, trackBar_hottime_MouseUp).
            return Ok(match command {
                Command::Start { burn } => vec![0xF1, check_burn(burn)?],
                Command::SetBurn(burn) => vec![check_burn(burn)?],
                Command::Pause => vec![0xF2],
                Command::Reset => vec![0xF9],
                Command::Home => vec![0xF3],
                Command::Center => vec![0xFB],
                Command::Box => vec![0xF4],
                Command::Move(d) => vec![0xF5, d as u8],
                Command::Backward => vec![0xFC, 0x55],
                Command::Forward => vec![0xFC, 0xAA],
                Command::Redo => vec![0xFC, 0x77],
                Command::Erase => vec![0xFE; 8],
                Command::MoveTo { .. } | Command::PowerOn | Command::PowerOff => {
                    return Err(unsupported());
                }
            });
        }
        // LaserCarver.Instruction: 4-byte frames FF cc pp pp.
        let has_navigation = matches!(self.version, Version::V20 | Version::V21);
        Ok(match command {
            Command::Start { burn } => {
                check_burn(burn)?;
                vec![0xFF, 1, 1, 0]
            }
            Command::SetBurn(burn) => vec![0xFF, 5, check_burn(burn)?, 0],
            Command::Pause => vec![0xFF, 1, 2, 0],
            Command::Redo => vec![0xFF, 1, 3, 0],
            Command::Reset | Command::PowerOn => vec![0xFF, 4, 1, 0],
            Command::PowerOff => vec![0xFF, 4, 2, 0],
            Command::Center => vec![0xFF, 2, 1, 0],
            Command::Box => vec![0xFF, 2, 2, 0],
            Command::Move(d) => vec![0xFF, 3, d as u8, 0],
            Command::Erase => vec![0xFF, 6, 1, 0],
            Command::Forward if has_navigation => vec![0xFF, 7, 2, 0],
            Command::Backward if has_navigation => vec![0xFF, 7, 1, 0],
            Command::MoveTo { x, y } => {
                let split = |v: u16| {
                    if v < 25_600 {
                        Ok([(v / 100) as u8, (v % 100) as u8])
                    } else {
                        Err(ProtocolError::CoordinateOutOfRange(v))
                    }
                };
                let ([xh, xl], [yh, yl]) = (split(x)?, split(y)?);
                vec![0xFF, 10, xh, xl, 0xFF, 11, yh, yl]
            }
            Command::Home | Command::Forward | Command::Backward => return Err(unsupported()),
        })
    }

    /// The bytes sent after the erase command for `base`, the `base_size`² image shown in
    /// the preview (black = burn).
    ///
    /// The image is pasted at the top left of a white canvas of [`Protocol::upload_size`].
    /// The original then flips it vertically (`RotateFlip(6)`) and saves a bottom-up BMP: the
    /// two cancel out, so the first data row is the top row of the image.
    /// - V10: the whole BMP file; bit 1 only for pure white (`To1bppImage`), so 0 = burn.
    /// - V2x/V30: Otsu (`ToBW`) on the canvas, then only the pixel data; 1 = burn.
    pub fn build_payload(self, base: &GrayImage) -> Vec<u8> {
        let (w, h) = self.upload_size();
        let mut canvas = GrayImage::from_pixel(w, h, image::Luma([255]));
        imaging::paste(&mut canvas, base, 0, 0);
        let payload = if self.version == Version::V10 {
            let data = bitmap::pack_1bpp(&canvas, |v| v == 255);
            bitmap::bmp_1bpp(w, h, &data)
        } else {
            // The flip does not change the histogram and every canvas width is a multiple
            // of 4 (no row padding), so thresholding before or after it is the same.
            let bw = imaging::bw_otsu(&canvas);
            bitmap::pack_1bpp(&bw, |v| v == 0)
        };
        debug_assert_eq!(payload.len(), self.payload_len());
        payload
    }
}

/// A message from the engraver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Reply to the handshake; V30 also reports its hardware version.
    Connected {
        hw: Option<u8>,
    },
    /// Engraving position in image pixels; -1 until the first coordinate arrives (V20/V21).
    Progress {
        x: i32,
        y: i32,
    },
    Finished,
    /// V10: no 5 V power supply ("Please insert the power supply").
    NoPower,
    PowerOk,
    EraseDone,
    /// Echo of the erase command (V2x/V30 `TransferImage`).
    TransferImage,
    Version(u8),
    LowBatterySleep,
    TimeoutSleep,
    Debug(u16),
    Battery(u8),
    BurnValue(u16),
    VerifyOk,
    /// V10 byte with no known meaning.
    Unknown(u8),
    /// The serial port was closed or unplugged (produced by the device layer).
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frame {
    ConnectAsk,
    TransferImage,
    Version,
    ProgressX,
    ProgressY,
    EraseDone,
    CarverDone,
    LowPowerSleep,
    OverTimeSleep,
    Debug,
    BatteryLevel,
    RecvFireRank,
    RecvOk,
}

/// `Instruction.RecvCommands`: first match wins, 0 = parameter byte.
const RECV_V20: [(Frame, [u8; 4]); 12] = [
    (Frame::ConnectAsk, [255, 1, 0, 0]),
    (Frame::TransferImage, [255, 6, 1, 0]),
    (Frame::Version, [255, 2, 0, 0]),
    (Frame::ProgressX, [255, 3, 0, 0]),
    (Frame::ProgressY, [255, 4, 0, 0]),
    (Frame::EraseDone, [255, 5, 1, 0]),
    (Frame::CarverDone, [255, 6, 0, 0]),
    (Frame::LowPowerSleep, [255, 7, 1, 0]),
    (Frame::OverTimeSleep, [255, 7, 2, 0]),
    (Frame::Debug, [255, 8, 0, 0]),
    (Frame::BatteryLevel, [255, 9, 0, 0]),
    (Frame::RecvFireRank, [255, 10, 0, 0]),
];

const RECV_V21: [(Frame, [u8; 4]); 12] = [
    (Frame::ConnectAsk, [255, 2, 100, 0]),
    (Frame::TransferImage, [255, 6, 1, 0]),
    (Frame::Version, [255, 2, 0, 0]),
    (Frame::ProgressX, [255, 3, 0, 0]),
    (Frame::ProgressY, [255, 4, 0, 0]),
    (Frame::EraseDone, [255, 5, 1, 0]),
    (Frame::CarverDone, [255, 6, 0, 0]),
    (Frame::LowPowerSleep, [255, 7, 1, 0]),
    (Frame::OverTimeSleep, [255, 7, 2, 0]),
    (Frame::Debug, [255, 8, 0, 0]),
    (Frame::BatteryLevel, [255, 9, 0, 0]),
    (Frame::RecvFireRank, [255, 10, 0, 0]),
];

const RECV_V30: [(Frame, [u8; 4]); 12] = [
    (Frame::ConnectAsk, [255, 2, 0, 0]),
    (Frame::TransferImage, [255, 6, 1, 0]),
    (Frame::ProgressX, [255, 3, 0, 0]),
    (Frame::ProgressY, [255, 4, 0, 0]),
    (Frame::EraseDone, [255, 5, 1, 0]),
    (Frame::CarverDone, [255, 6, 0, 0]),
    (Frame::LowPowerSleep, [255, 7, 1, 0]),
    (Frame::OverTimeSleep, [255, 7, 2, 0]),
    (Frame::Debug, [255, 8, 0, 0]),
    (Frame::BatteryLevel, [255, 9, 0, 0]),
    (Frame::RecvFireRank, [255, 10, 0, 0]),
    (Frame::RecvOk, [255, 11, 0, 0]),
];

/// Turns the byte stream from the engraver into [`Event`]s. Bytes may arrive split in any
/// way: incomplete messages wait for the next [`Parser::feed`].
#[derive(Clone, Debug)]
pub struct Parser {
    protocol: Protocol,
    buf: Vec<u8>,
    x: i32,
    y: i32,
}

impl Parser {
    pub fn new(protocol: Protocol) -> Self {
        Parser { protocol, buf: Vec::new(), x: -1, y: -1 }
    }

    /// The protocol, with the hardware version updated by the last V30 `Connected`.
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Event> {
        match self.protocol.version {
            Version::V10 => self.feed_v10(data),
            _ => self.feed_4byte(data),
        }
    }

    /// `serialPort_Laser_DataReceived`: `FF` + 4 bytes = position, single bytes = states.
    fn feed_v10(&mut self, data: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &b in data {
            if b == 0xFF || !self.buf.is_empty() {
                self.buf.push(b);
                if self.buf.len() == 5 {
                    let b = &self.buf;
                    let x = i32::from(b[1]) * 100 + i32::from(b[2]);
                    let y = i32::from(b[3]) * 100 + i32::from(b[4]);
                    events.push(Event::Progress { x, y });
                    self.buf.clear();
                }
                continue;
            }
            events.push(match b {
                0x65 => Event::Connected { hw: None },
                0x66 => Event::Finished,
                0x78 => Event::NoPower,
                0x79 => Event::PowerOk,
                other => Event::Unknown(other),
            });
        }
        events
    }

    fn feed_4byte(&mut self, data: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(data);
        let mut events = Vec::new();
        loop {
            let Some(start) = self.buf.iter().position(|&b| b == 0xFF) else {
                self.buf.clear();
                break;
            };
            self.buf.drain(..start);
            if self.buf.len() < 4 {
                break;
            }
            let frame = [self.buf[0], self.buf[1], self.buf[2], self.buf[3]];
            let Some((kind, params)) = self.match_frame(frame) else {
                // The original empties the whole buffer here; dropping one byte resyncs
                // on the next 0xFF without losing a following valid frame.
                self.buf.remove(0);
                continue;
            };
            self.buf.drain(..4);
            if let Some(event) = self.frame_event(kind, params) {
                events.push(event);
            }
        }
        events
    }

    fn match_frame(&self, frame: [u8; 4]) -> Option<(Frame, [u8; 2])> {
        let table: &[(Frame, [u8; 4])] = match self.protocol.version {
            Version::V20 => &RECV_V20,
            Version::V21 => &RECV_V21,
            _ => &RECV_V30,
        };
        'patterns: for &(kind, pattern) in table {
            let mut params = [0u8; 2];
            let mut n = 0;
            for k in 1..4 {
                if pattern[k] == 0 {
                    if n < 2 {
                        params[n] = frame[k];
                    }
                    n += 1;
                } else if pattern[k] != frame[k] {
                    continue 'patterns;
                }
            }
            return Some((kind, params));
        }
        None
    }

    fn frame_event(&mut self, kind: Frame, p: [u8; 2]) -> Option<Event> {
        let pair = i32::from(p[0]) * 100 + i32::from(p[1]);
        let v30 = self.protocol.version == Version::V30;
        Some(match kind {
            Frame::ConnectAsk if v30 => {
                self.protocol.hw = Some(p[0]);
                Event::Connected { hw: Some(p[0]) }
            }
            Frame::ConnectAsk => Event::Connected { hw: None },
            Frame::Version => {
                self.protocol.hw = Some(p[0]);
                Event::Version(p[0])
            }
            Frame::ProgressX if v30 => {
                self.x = pair;
                return self.v30_point();
            }
            Frame::ProgressY if v30 => {
                self.y = pair;
                return self.v30_point();
            }
            // V20/V21: X is off by one, the Y that arrived before it completes the point.
            Frame::ProgressX => {
                self.x = (pair - 1).max(0);
                Event::Progress { x: self.x, y: self.y }
            }
            Frame::ProgressY => {
                self.y = pair;
                return None;
            }
            Frame::EraseDone => Event::EraseDone,
            Frame::CarverDone => Event::Finished,
            Frame::LowPowerSleep => Event::LowBatterySleep,
            Frame::OverTimeSleep => Event::TimeoutSleep,
            Frame::Debug => Event::Debug(pair as u16),
            Frame::BatteryLevel => Event::Battery(p[0]),
            Frame::RecvFireRank => Event::BurnValue(pair as u16),
            Frame::RecvOk => Event::VerifyOk,
            Frame::TransferImage => Event::TransferImage,
        })
    }

    /// V30 sends X and Y in either order; a point is complete when both are in range.
    fn v30_point(&mut self) -> Option<Event> {
        let n = self.protocol.base_size() as i32;
        if (0..n).contains(&self.x) && (0..n).contains(&self.y) {
            let event = Event::Progress { x: self.x, y: self.y };
            self.x = -1;
            self.y = -1;
            return Some(event);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker() -> GrayImage {
        let mut im = GrayImage::from_pixel(490, 490, image::Luma([255]));
        for y in 10..40 {
            for x in 10..60 {
                im.put_pixel(x, y, image::Luma([0]));
            }
        }
        im
    }

    #[test]
    fn v10_commands() {
        let p = Protocol::new(Version::V10, None);
        assert_eq!(p.handshake(), [0xF6]);
        assert_eq!(p.encode(Command::Start { burn: 30 }).unwrap(), [0xF1, 0x1E]);
        assert_eq!(p.encode(Command::Erase).unwrap(), [0xFE; 8]);
        assert_eq!(p.encode(Command::Move(Direction::Left)).unwrap(), [0xF5, 0x03]);
        assert_eq!(p.encode(Command::Start { burn: 121 }), Err(ProtocolError::BurnOutOfRange(121)));
        assert!(matches!(p.encode(Command::PowerOn), Err(ProtocolError::Unsupported { .. })));
    }

    #[test]
    fn four_byte_commands() {
        let p = Protocol::new(Version::V30, Some(11));
        assert_eq!(p.encode(Command::SetBurn(45)).unwrap(), [0xFF, 5, 45, 0]);
        assert_eq!(p.encode(Command::MoveTo { x: 123, y: 405 }).unwrap(), [0xFF, 10, 1, 23, 0xFF, 11, 4, 5]);
        assert!(p.encode(Command::Forward).is_err());
        assert_eq!(Protocol::new(Version::V20, None).encode(Command::Forward).unwrap(), [0xFF, 7, 2, 0]);
    }

    #[test]
    fn v10_payload_shape_and_orientation() {
        let data = Protocol::new(Version::V10, None).build_payload(&marker());
        assert_eq!(data.len(), V10_PAYLOAD_LEN);
        assert_eq!(&data[..2], b"BM");
        let pix = &data[62..];
        let row10 = &pix[10 * 64..11 * 64];
        assert_eq!((row10[1] >> (7 - 2)) & 1, 0, "x = 10 must burn (0)");
        assert_eq!(row10[0] >> 7, 1, "x = 0 is white (1)");
        assert!(pix[511 * 64..].iter().all(|&b| b == 0xFF), "outside the 490 px area is white");
    }

    #[test]
    fn four_byte_payload_sizes() {
        for (version, hw, len) in
            [(Version::V20, None, 39_744), (Version::V21, None, 32_768), (Version::V30, Some(11), 32_768)]
        {
            let p = Protocol::new(version, hw);
            let n = p.base_size();
            let base = image::imageops::resize(&marker(), n, n, image::imageops::FilterType::Nearest);
            let data = p.build_payload(&base);
            assert_eq!(data.len(), len);
            let stride = p.upload_size().0 as usize / 8;
            assert_ne!(data[20 * stride + 2] & 0x08, 0, "black = 1 in the newer protocols");
            assert_eq!(data[300 * stride + 30], 0);
        }
        assert_eq!(Protocol::new(Version::V30, Some(102)).payload_len(), 72 * 572);
        assert_eq!(Protocol::new(Version::V30, Some(12)).base_size(), 550);
    }

    #[test]
    fn recognise_replies() {
        assert_eq!(Version::V30.recognise(&[255, 2, 13, 1]), Some(Protocol::new(Version::V30, Some(13))));
        assert_eq!(Version::V10.recognise(&[0x79, 0x65]), Some(Protocol::new(Version::V10, None)));
        assert_eq!(Version::V21.recognise(&[255, 2, 13, 1]), None);
    }

    #[test]
    fn v10_parser_handles_split_input() {
        let mut parser = Parser::new(Protocol::new(Version::V10, None));
        let mut events = parser.feed(&[0x65, 0xFF, 1, 23]);
        events.extend(parser.feed(&[4, 5, 0x66, 0x78, 0x79]));
        assert_eq!(
            events,
            [
                Event::Connected { hw: None },
                Event::Progress { x: 123, y: 405 },
                Event::Finished,
                Event::NoPower,
                Event::PowerOk
            ]
        );
    }

    #[test]
    fn v30_parser() {
        let mut parser = Parser::new(Protocol::new(Version::V30, None));
        let events = parser.feed(&[255, 2, 11, 1, 255, 5, 1, 0, 255, 3, 1, 2, 255, 4, 0, 50, 7, 255, 6, 0, 0]);
        assert_eq!(
            events,
            [Event::Connected { hw: Some(11) }, Event::EraseDone, Event::Progress { x: 102, y: 50 }, Event::Finished]
        );
        assert_eq!(parser.protocol().hw, Some(11));
    }
}
