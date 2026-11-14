//! Serial link to the engraver: port discovery, protocol detection and the session.
//!
//! The Windows "driver" shipped by NEJE is only the WCH CH341SER driver of the CH340
//! USB-serial converter (VID 1A86, PID 7523). On Linux the kernel module `ch341` handles
//! it: the engraver shows up as `/dev/ttyUSB0` with nothing to install.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::GrayImage;
use serialport::{DataBits, FlowControl, Parity, SerialPort, SerialPortType, StopBits};

use crate::protocol::{BAUD_RATE, Command, Event, Parser, Protocol, ProtocolError, Version};

/// USB IDs of the CH340/CH341 converters handled by the Linux `ch341` driver.
pub const CH340_IDS: [(u16, u16); 4] = [(0x1A86, 0x7523), (0x1A86, 0x5523), (0x4348, 0x5523), (0x1A86, 0x7522)];

/// The image goes out as one continuous stream, like the single `Write` of V10.exe; it is
/// cut in blocks (the `WriteBufferSize` of the launcher) only to report the progress.
const UPLOAD_CHUNK: usize = 1024;
/// Pause between the last byte of an image and a start command sent right after it. The
/// original always has a human click in between; whether the firmware needs time to finish
/// writing its flash is not known yet, so this value is a guess to be tuned on the machine.
pub const AFTER_UPLOAD_PAUSE: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_millis(50);
/// How long each handshake waits for its reply during detection.
const PROBE_WAIT: Duration = Duration::from_millis(150);
/// How long a new session waits for the reply to its handshake.
const CONNECT_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("cannot open {port}: {source}{hint}", hint = access_hint(.source))]
    Open { port: String, source: serialport::Error },
    #[error("cannot list the serial ports: {0}")]
    List(serialport::Error),
    #[error("engraver not found{}", not_found_details(.tried))]
    NotFound { tried: Vec<(String, Option<String>)> },
    #[error("serial port error: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("the engraver did not confirm the erase (Erase Err): try again")]
    EraseTimeout,
}

fn access_hint(source: &serialport::Error) -> &'static str {
    if matches!(source.kind(), serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied)) {
        " (install the udev rule of this project, or add your user to the group that owns the port: \"uucp\" on Arch, \"dialout\" on Debian/Ubuntu)"
    } else {
        ""
    }
}

fn not_found_details(tried: &[(String, Option<String>)]) -> String {
    if tried.is_empty() {
        return ": no CH340 serial port found. Check the USB cable, or name the port with --port".to_owned();
    }
    let ports: Vec<String> = tried
        .iter()
        .map(|(port, error)| match error {
            Some(error) => format!("{port} ({error})"),
            None => format!("{port} (no reply)"),
        })
        .collect();
    format!(". Check the USB cable and the 5 V power supply. Ports tried: {}", ports.join(", "))
}

/// A serial port of this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
    pub name: String,
    /// USB vendor and product ID, for USB converters.
    pub usb: Option<(u16, u16)>,
    pub description: String,
}

impl PortInfo {
    /// True for the converter the engraver uses.
    pub fn is_ch340(&self) -> bool {
        self.usb.is_some_and(|id| CH340_IDS.contains(&id))
    }
}

/// Serial ports, CH340 converters first.
pub fn ports() -> Result<Vec<PortInfo>, DeviceError> {
    let mut ports: Vec<PortInfo> = serialport::available_ports()
        .map_err(DeviceError::List)?
        .into_iter()
        .map(|p| match p.port_type {
            SerialPortType::UsbPort(usb) => PortInfo {
                name: p.port_name,
                usb: Some((usb.vid, usb.pid)),
                description: [usb.manufacturer, usb.product].into_iter().flatten().collect::<Vec<_>>().join(" "),
            },
            _ => PortInfo { name: p.port_name, usb: None, description: String::new() },
        })
        .collect();
    ports.sort_by_key(|p| (!p.is_ch340(), p.name.clone()));
    Ok(ports)
}

fn open(port: &str) -> Result<Box<dyn SerialPort>, DeviceError> {
    let mut serial = serialport::new(port, BAUD_RATE)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(READ_TIMEOUT)
        .open()
        .map_err(|source| DeviceError::Open { port: port.to_owned(), source })?;
    // Like .NET SerialPort (DtrEnable = RtsEnable = false). Pseudo-terminals have no modem
    // lines and refuse the request, which is harmless.
    let _ = serial.write_data_terminal_ready(false);
    let _ = serial.write_request_to_send(false);
    Ok(serial)
}

fn debug_traffic(direction: &str, data: &[u8]) {
    static ENABLED: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("NEJE_DEBUG").is_some_and(|v| v == "1"));
    if *ENABLED {
        let hex: Vec<String> = data.iter().take(64).map(|b| format!("{b:02X}")).collect();
        let more = if data.len() > 64 { format!(" ... ({} bytes)", data.len()) } else { String::new() };
        eprintln!("[neje] {direction} {}{more}", hex.join(" "));
    }
}

fn write_all(serial: &mut dyn SerialPort, data: &[u8]) -> io::Result<()> {
    debug_traffic("->", data);
    serial.write_all(data)?;
    serial.flush()
}

/// Reads until `deadline`, returning what arrived. Timeouts are not errors.
fn read_until(
    serial: &mut dyn SerialPort,
    deadline: Instant,
    mut done: impl FnMut(&[u8]) -> bool,
) -> io::Result<Vec<u8>> {
    let mut reply = Vec::new();
    let mut chunk = [0u8; 64];
    while Instant::now() < deadline {
        match serial.read(&mut chunk) {
            Ok(n) => {
                debug_traffic("<-", &chunk[..n]);
                reply.extend_from_slice(&chunk[..n]);
                if done(&reply) {
                    break;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(reply)
}

/// Asks the device on `port` to identify itself, trying `versions` in order.
///
/// Each step sends only the handshake of the versions being tried: `F6` for V10 and
/// `FF 09 00 00` once for V20/V21/V30, which share it and differ in the reply. Detection
/// stops at the first recognised reply, so a V10 machine never sees the 4-byte handshake.
pub fn probe(port: &str, versions: &[Version]) -> Result<Option<Protocol>, DeviceError> {
    let mut serial = open(port)?;
    thread::sleep(Duration::from_millis(50));
    serial.clear(serialport::ClearBuffer::Input).map_err(io::Error::from)?;
    let mut tried: Vec<&[u8]> = Vec::new();
    for &version in versions {
        let handshake = version.handshake();
        if tried.contains(&handshake) {
            continue;
        }
        tried.push(handshake);
        let candidates: Vec<Version> = versions.iter().copied().filter(|v| v.handshake() == handshake).collect();
        let recognise = |reply: &[u8]| candidates.iter().find_map(|v| v.recognise(reply));
        write_all(serial.as_mut(), handshake)?;
        let reply = read_until(serial.as_mut(), Instant::now() + PROBE_WAIT, |reply| recognise(reply).is_some())?;
        if let Some(protocol) = recognise(&reply) {
            return Ok(Some(protocol));
        }
        thread::sleep(Duration::from_millis(20));
        serial.clear(serialport::ClearBuffer::Input).map_err(io::Error::from)?;
    }
    Ok(None)
}

/// Finds the engraver by trying `versions` in order (see [`probe`]). Without `port` only
/// CH340 converters are probed, so the handshake is never sent to an unrelated serial
/// device; name the port to try anything else.
///
/// Pass only [`Version::V10`] for a DK-8-KZ: the 4-byte handshake of the newer protocols
/// reads as "burn time 9" to a V10 firmware that did not answer in time.
pub fn detect(port: Option<&str>, versions: &[Version]) -> Result<(String, Protocol), DeviceError> {
    let candidates: Vec<String> = match port {
        Some(port) => vec![port.to_owned()],
        None => ports()?.into_iter().filter(PortInfo::is_ch340).map(|p| p.name).collect(),
    };
    let mut tried = Vec::new();
    for port in candidates {
        match probe(&port, versions) {
            Ok(Some(protocol)) => return Ok((port, protocol)),
            Ok(None) => tried.push((port, None)),
            Err(e) => tried.push((port, Some(e.to_string()))),
        }
    }
    Err(DeviceError::NotFound { tried })
}

#[derive(Debug)]
struct State {
    protocol: Protocol,
    connected: bool,
    erase_done: bool,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock only means another thread panicked; the flags are still valid.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits until `ready(state)` or the timeout; returns whether it became ready.
    fn wait_for(&self, timeout: Duration, ready: impl Fn(&State) -> bool) -> bool {
        let guard = self.lock();
        let (guard, _) =
            self.changed.wait_timeout_while(guard, timeout, |s| !ready(s)).unwrap_or_else(|e| e.into_inner());
        ready(&guard)
    }
}

/// An open session with the engraver. Incoming messages are parsed by a reader thread and
/// queued in [`Engraver::events`]. Dropping the session closes the port.
pub struct Engraver {
    port: String,
    writer: Mutex<Box<dyn SerialPort>>,
    events: Receiver<Event>,
    shared: Arc<Shared>,
    acknowledged: bool,
    reader: Option<JoinHandle<()>>,
}

impl Engraver {
    /// Finds the engraver with [`detect`] and opens a session with it. `hw` replaces the
    /// hardware version when the engraver does not report one.
    pub fn connect(port: Option<&str>, versions: &[Version], hw: Option<u8>) -> Result<Self, DeviceError> {
        let (port, mut protocol) = detect(port, versions)?;
        protocol.hw = protocol.hw.or(hw);
        Self::open(&port, protocol)
    }

    /// Opens a session on `port` without detecting anything: the handshake is sent once and
    /// [`Engraver::acknowledged`] tells whether the engraver answered. Useful when the
    /// firmware may be too busy to answer, e.g. to pause a running engraving.
    pub fn open(port: &str, protocol: Protocol) -> Result<Self, DeviceError> {
        let writer = open(port)?;
        let reader_port = writer.try_clone().map_err(|source| DeviceError::Open { port: port.to_owned(), source })?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State { protocol, connected: false, erase_done: false }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let (tx, events) = mpsc::channel();
        let reader = thread::spawn({
            let shared = Arc::clone(&shared);
            move || read_loop(reader_port, Parser::new(protocol), &shared, &tx)
        });
        let mut engraver = Engraver {
            port: port.to_owned(),
            writer: Mutex::new(writer),
            events,
            shared,
            acknowledged: false,
            reader: Some(reader),
        };
        // Repeat the handshake on the open session, like FormOld/FormMain.
        engraver.write(protocol.handshake())?;
        engraver.acknowledged = engraver.shared.wait_for(CONNECT_WAIT, |s| s.connected);
        Ok(engraver)
    }

    pub fn port(&self) -> &str {
        &self.port
    }

    /// The protocol in use, with the hardware version the engraver reported.
    pub fn protocol(&self) -> Protocol {
        self.shared.lock().protocol
    }

    /// False when the engraver did not answer the handshake of this session.
    pub fn acknowledged(&self) -> bool {
        self.acknowledged
    }

    /// Messages from the engraver, in order of arrival.
    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    fn write(&self, data: &[u8]) -> Result<(), DeviceError> {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        Ok(write_all(writer.as_mut(), data)?)
    }

    pub fn send(&self, command: Command) -> Result<(), DeviceError> {
        self.write(&self.protocol().encode(command)?)
    }

    /// Starts engraving the image in memory with burn time `burn`. SWITCHES THE LASER ON.
    pub fn start(&self, burn: u8) -> Result<(), DeviceError> {
        if self.protocol().version != Version::V10 {
            self.send(Command::SetBurn(burn))?;
            thread::sleep(Duration::from_millis(50));
        }
        self.send(Command::Start { burn })
    }

    /// Erases the engraver memory and sends `base` (a `base_size`² image, black = burn).
    /// `erase_wait` overrides the wait after the erase command; `progress(sent, total)` is
    /// called after every block.
    pub fn upload(
        &self,
        base: &GrayImage,
        erase_wait: Option<Duration>,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<(), DeviceError> {
        let protocol = self.protocol();
        let payload = protocol.build_payload(base);
        let wait = erase_wait.unwrap_or(protocol.erase_wait());
        self.shared.lock().erase_done = false;
        self.send(Command::Erase)?;
        if !protocol.confirms_erase() {
            // V10 gives no confirmation and loses whatever arrives while it erases.
            thread::sleep(wait);
        } else if !self.shared.wait_for(wait, |s| s.erase_done) {
            return Err(DeviceError::EraseTimeout);
        }
        let mut sent = 0;
        for chunk in payload.chunks(UPLOAD_CHUNK) {
            self.write(chunk)?;
            sent += chunk.len();
            progress(sent, payload.len());
        }
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    /// Waits for an event accepted by `until`, passing every event to `on_event` first.
    /// Returns `None` when `timeout` expires or `keep_waiting()` turns false.
    pub fn wait_for(
        &self,
        until: impl Fn(&Event) -> bool,
        timeout: Option<Duration>,
        mut on_event: impl FnMut(&Event),
        keep_waiting: impl Fn() -> bool,
    ) -> Option<Event> {
        let deadline = timeout.map(|t| Instant::now() + t);
        while keep_waiting() {
            let mut step = Duration::from_millis(200);
            if let Some(deadline) = deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return None;
                }
                step = step.min(left);
            }
            match self.events.recv_timeout(step) {
                Ok(event) => {
                    on_event(&event);
                    if until(&event) {
                        return Some(event);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Some(Event::Disconnected),
            }
        }
        None
    }
}

impl Drop for Engraver {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            // The reader checks the flag every READ_TIMEOUT; a panic in it is not ours to report.
            let _ = reader.join();
        }
    }
}

fn read_loop(mut port: Box<dyn SerialPort>, mut parser: Parser, shared: &Shared, events: &Sender<Event>) {
    let mut buf = [0u8; 256];
    while !shared.stop.load(Ordering::Relaxed) {
        let n = match port.read(&mut buf) {
            Ok(0) => {
                thread::sleep(READ_TIMEOUT);
                continue;
            }
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(_) => {
                // The receiver may already be gone when the session is being dropped.
                let _ = events.send(Event::Disconnected);
                return;
            }
        };
        debug_traffic("<-", &buf[..n]);
        for event in parser.feed(&buf[..n]) {
            {
                let mut state = shared.lock();
                state.protocol = parser.protocol();
                match event {
                    Event::Connected { .. } => state.connected = true,
                    Event::EraseDone => state.erase_done = true,
                    _ => {}
                }
            }
            shared.changed.notify_all();
            if events.send(event).is_err() {
                return;
            }
        }
    }
}
