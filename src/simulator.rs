//! Virtual engraver on a pseudo-terminal, to try the software without the machine.
//!
//! It behaves like the V10 firmware measured on a DK-8-KZ (or as the V30 one is expected
//! to): answers the handshake, discards what arrives while it erases, decodes the received
//! image ("virtual engraving"), reports the position of every burnt pixel, row by row in a
//! zig-zag, and the end of the job. It never does more than the real
//! protocol allows, so mistakes such as sending the image too early show up in
//! [`Report::errors`].

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::GrayImage;
use serialport::{SerialPort, TTYPort};

use crate::bitmap::{self, BMP_HEADER_LEN};
use crate::protocol::{Protocol, V10_PAYLOAD_LEN, Version};

#[derive(Clone, Debug)]
pub struct Options {
    /// `V10`, or any other version for the 4-byte protocol (answers as V30).
    pub version: Version,
    /// Hardware version reported by the 4-byte protocol.
    pub hw: u8,
    /// How long the erase takes; data arriving meanwhile is lost, as on the machine.
    pub erase_time: Duration,
    /// Where to save the received image.
    pub out_png: Option<PathBuf>,
    /// Pause after each burnt pixel (zero: as fast as possible).
    pub pixel_delay: Duration,
    /// Restart the board after this many burnt pixels, as a weak power supply does: it
    /// sends `79` and the job never ends.
    pub restart_after: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            version: Version::V10,
            hw: 11,
            erase_time: Duration::from_millis(1500),
            out_png: None,
            pixel_delay: Duration::ZERO,
            restart_after: None,
        }
    }
}

/// What the virtual engraver has seen so far.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Commands received, one entry per command.
    pub received: Vec<Vec<u8>>,
    /// Last burn time set.
    pub burn: Option<u8>,
    /// Last image received, decoded (0 = burn).
    pub image: Option<GrayImage>,
    /// The image in memory at each start, in order.
    pub jobs: Vec<GrayImage>,
    /// Protocol violations, in plain words.
    pub errors: Vec<String>,
    /// Movement and control commands, as hex strings.
    pub moves: Vec<String>,
    /// How many times an engraving was started.
    pub started: u32,
}

enum Phase {
    Idle,
    Erasing { until: Instant },
    Receiving,
}

pub struct Simulator {
    port: String,
    report: Arc<Mutex<Report>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    // Keeps the pseudo-terminal alive while no client has it open.
    _slave: TTYPort,
}

impl Simulator {
    /// Creates the pseudo-terminal and starts answering on it.
    pub fn start(options: Options) -> io::Result<Self> {
        let (mut master, slave) = TTYPort::pair().map_err(io::Error::from)?;
        master.set_timeout(Duration::from_millis(50)).map_err(io::Error::from)?;
        let port = slave.name().ok_or_else(|| io::Error::other("the pseudo-terminal has no name"))?;
        let report = Arc::new(Mutex::new(Report::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut machine = Machine {
            options,
            master,
            report: Arc::clone(&report),
            stop: Arc::clone(&stop),
            buf: Vec::new(),
            phase: Phase::Idle,
            expect: 0,
            image: Vec::new(),
        };
        let thread = thread::spawn(move || machine.run());
        Ok(Simulator { port, report, stop, thread: Some(thread), _slave: slave })
    }

    /// Path of the serial port to give to the software under test.
    pub fn port(&self) -> &str {
        &self.port
    }

    /// Snapshot of what the virtual engraver has seen.
    pub fn report(&self) -> Report {
        lock(&self.report).clone()
    }

    /// Returns the errors seen since the last call.
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.report).errors)
    }
}

impl Drop for Simulator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // The loop checks the flag every 50 ms; a panic in it is not ours to report.
            let _ = thread.join();
        }
    }
}

fn lock(report: &Mutex<Report>) -> MutexGuard<'_, Report> {
    // A poisoned lock only means a simulator thread panicked; the report is still readable.
    report.lock().unwrap_or_else(|e| e.into_inner())
}

/// Writes to the software under test. Errors mean it closed the port: nothing to do.
fn reply(master: &mut TTYPort, data: &[u8]) {
    let _ = master.write_all(data);
}

struct Machine {
    options: Options,
    master: TTYPort,
    report: Arc<Mutex<Report>>,
    stop: Arc<AtomicBool>,
    buf: Vec<u8>,
    phase: Phase,
    expect: usize,
    image: Vec<u8>,
}

impl Machine {
    fn is_v10(&self) -> bool {
        self.options.version == Version::V10
    }

    fn error(&self, message: String) {
        lock(&self.report).errors.push(message);
    }

    fn run(&mut self) {
        let mut chunk = [0u8; 4096];
        while !self.stop.load(Ordering::Relaxed) {
            let n = match self.master.read(&mut chunk) {
                Ok(n) if n > 0 => n,
                // Timeout, or no client on the other end yet (EIO): wait and retry.
                _ => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
            };
            let mut data = &chunk[..n];
            if let Phase::Erasing { until } = self.phase {
                if Instant::now() < until {
                    self.error(format!("{} bytes received DURING the erase (lost)", data.len()));
                    continue;
                }
                self.phase = Phase::Receiving;
            }
            if matches!(self.phase, Phase::Receiving) {
                let take = data.len().min(self.expect - self.image.len());
                self.image.extend_from_slice(&data[..take]);
                data = &data[take..];
                if self.image.len() >= self.expect {
                    self.finish_image();
                    self.phase = Phase::Idle;
                }
                if data.is_empty() {
                    continue;
                }
            }
            self.buf.extend_from_slice(data);
            if self.is_v10() { self.parse_v10() } else { self.parse_4byte() }
        }
    }

    fn record(&mut self, len: usize, is_move: bool) -> Vec<u8> {
        let command: Vec<u8> = self.buf.drain(..len).collect();
        let mut report = lock(&self.report);
        if is_move {
            report.moves.push(command.iter().map(|b| format!("{b:02x}")).collect());
        }
        report.received.push(command.clone());
        command
    }

    fn parse_v10(&mut self) {
        while let Some(&c) = self.buf.first() {
            match c {
                0xF6 => {
                    self.record(1, false);
                    reply(&mut self.master, &[0x65, 0x70]); // what a real DK-8-KZ answers
                }
                0xFE => {
                    if self.buf.len() < 8 {
                        return;
                    }
                    if self.buf[..8] != [0xFE; 8] {
                        self.error("incomplete erase sequence".to_owned());
                        self.buf.remove(0);
                        continue;
                    }
                    self.record(8, false);
                    self.begin_upload(V10_PAYLOAD_LEN);
                    if !self.buf.is_empty() {
                        self.error(format!("{} bytes received DURING the erase (lost)", self.buf.len()));
                        self.buf.clear();
                    }
                    return;
                }
                0xF1 => {
                    if self.buf.len() < 2 {
                        return;
                    }
                    let command = self.record(2, false);
                    lock(&self.report).burn = Some(command[1]);
                    self.engrave();
                }
                0xF5 | 0xFC => {
                    if self.buf.len() < 2 {
                        return;
                    }
                    self.record(2, true);
                }
                0xF2 | 0xF3 | 0xF4 | 0xF9 | 0xFB => {
                    self.record(1, true);
                    if c == 0xF9 {
                        reply(&mut self.master, &[0x79]); // restarted
                    }
                }
                1..=0xF0 => {
                    self.record(1, false);
                    lock(&self.report).burn = Some(c);
                }
                _ => {
                    self.error(format!("unknown byte {c:#04x}"));
                    self.buf.remove(0);
                }
            }
        }
    }

    fn parse_4byte(&mut self) {
        while self.buf.len() >= 4 {
            if self.buf[0] != 0xFF {
                self.error(format!("byte outside a frame {:#04x}", self.buf[0]));
                self.buf.remove(0);
                continue;
            }
            let frame = [self.buf[0], self.buf[1], self.buf[2], self.buf[3]];
            match frame {
                [255, 9, 0, 0] => {
                    self.record(4, false);
                    reply(&mut self.master, &[255, 2, self.options.hw, 1]);
                }
                [255, 6, 1, 0] => {
                    self.record(4, false);
                    let protocol = Protocol::new(Version::V30, Some(self.options.hw));
                    self.begin_upload(protocol.payload_len());
                    self.confirm_erase_later();
                    if !self.buf.is_empty() {
                        self.error("data received DURING the erase (lost)".to_owned());
                        self.buf.clear();
                    }
                    return;
                }
                [255, 5, burn, _] => {
                    self.record(4, false);
                    lock(&self.report).burn = Some(burn);
                }
                [255, 1, 1, 0] => {
                    self.record(4, false);
                    self.engrave();
                }
                _ => {
                    self.record(4, true);
                }
            }
        }
    }

    fn begin_upload(&mut self, expect: usize) {
        self.phase = Phase::Erasing { until: Instant::now() + self.options.erase_time };
        self.expect = expect;
        self.image.clear();
    }

    /// The 4-byte firmware confirms the erase (`FF 05 01 00`) when it is done.
    fn confirm_erase_later(&self) {
        let Ok(mut master) = self.master.try_clone_native() else {
            return;
        };
        let delay = self.options.erase_time;
        thread::spawn(move || {
            thread::sleep(delay);
            reply(&mut master, &[255, 5, 1, 0]);
        });
    }

    fn finish_image(&mut self) {
        let (pixels, burn_bit, (w, h)) = if self.is_v10() {
            if !self.image.starts_with(b"BM") {
                self.error("BMP header missing".to_owned());
            }
            (&self.image[BMP_HEADER_LEN.min(self.image.len())..], 0, (512, 512))
        } else {
            (&self.image[..], 1, Protocol::new(Version::V30, Some(self.options.hw)).upload_size())
        };
        let im = bitmap::unpack_1bpp(pixels, w, h, burn_bit);
        if let Some(path) = &self.options.out_png
            && let Err(e) = im.save(path)
        {
            self.error(format!("cannot save {}: {e}", path.display()));
        }
        lock(&self.report).image = Some(im);
    }

    /// Reports every burnt pixel as a position, then the end of the job.
    fn engrave(&mut self) {
        let image = {
            let mut report = lock(&self.report);
            report.started += 1;
            match report.image.clone() {
                Some(image) => report.jobs.push(image.clone()),
                None => report.errors.push("start without an image".to_owned()),
            }
            report.image.clone()
        };
        let Ok(mut master) = self.master.try_clone_native() else {
            return;
        };
        let v10 = self.is_v10();
        let (pixel_delay, restart_after) = (self.options.pixel_delay, self.options.restart_after);
        let stop = Arc::clone(&self.stop);
        thread::spawn(move || {
            let points = image.as_ref().map(scan_order).unwrap_or_default();
            for (n, (x, y)) in points.into_iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if restart_after == Some(n) {
                    reply(&mut master, &[0x79]);
                    return;
                }
                let (xh, xl, yh, yl) = ((x / 100) as u8, (x % 100) as u8, (y / 100) as u8, (y % 100) as u8);
                if v10 {
                    reply(&mut master, &[0xFF, xh, xl, yh, yl]);
                } else {
                    reply(&mut master, &[255, 3, xh, xl, 255, 4, yh, yl]);
                }
                if !pixel_delay.is_zero() {
                    thread::sleep(pixel_delay);
                }
            }
            reply(&mut master, if v10 { &[0x66] } else { &[255, 6, 0, 0] });
        });
    }
}

/// The burnt pixels of the 490×490 area in the order the firmware engraves them: rows from
/// the top, alternating direction (the first one right to left), white rows skipped.
fn scan_order(image: &GrayImage) -> Vec<(u32, u32)> {
    let side = 490.min(image.width().min(image.height()));
    let mut points = Vec::new();
    let mut leftwards = true;
    for y in 0..side {
        let mut row: Vec<(u32, u32)> = (0..side).filter(|&x| image.get_pixel(x, y).0[0] == 0).map(|x| (x, y)).collect();
        if row.is_empty() {
            continue;
        }
        if leftwards {
            row.reverse();
        }
        leftwards = !leftwards;
        points.extend(row);
    }
    points
}
