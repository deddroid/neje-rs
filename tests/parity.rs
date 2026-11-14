//! Byte-for-byte comparison with the Python prototype.
//!
//! `tests/parity/reference.py` runs the prototype and writes a manifest with the SHA-256 of
//! every expected output; this test recomputes each case with the Rust library.
//!
//! ```sh
//! python tests/parity/reference.py --prototype ../neje-linux --out /tmp/parity \
//!     --clipart ../Image --samples "../Neje - 3.6/pic sample 490x490px"
//! NEJE_PARITY_DIR=/tmp/parity cargo test --test parity -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use image::GrayImage;
use neje::imaging::{self, Mode};
use neje::protocol::{Command, Direction, Event, Parser, Protocol, ProtocolError, Version};
use sha2::{Digest, Sha256};

fn sha(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn mode(s: &str) -> Mode {
    match s {
        "bw" => Mode::Bw,
        "dither" => Mode::Dither,
        "gray-threshold" => Mode::GrayThreshold,
        other => panic!("unknown mode {other}"),
    }
}

fn protocol(version: &str, hw: &str) -> Protocol {
    let hw: u8 = hw.parse().unwrap();
    Protocol::new(version.parse().unwrap(), (hw != 0).then_some(hw))
}

fn base_of(path: &str, base_size: u32, m: Mode) -> GrayImage {
    let picture = imaging::prepare(&imaging::load(Path::new(path)).unwrap(), base_size, m);
    imaging::center_on(&imaging::new_base(base_size), &picture)
}

/// Python's `float.hex()` output back to a float.
fn float_from_hex(s: &str) -> f64 {
    let (negative, s) = s.strip_prefix('-').map_or((false, s), |rest| (true, rest));
    let s = s.strip_prefix("0x").unwrap();
    let (mantissa, exponent) = s.split_once('p').unwrap();
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut value = u64::from_str_radix(int, 16).unwrap() as f64;
    for (i, digit) in frac.chars().enumerate() {
        value += f64::from(digit.to_digit(16).unwrap()) * 16f64.powi(-(i as i32 + 1));
    }
    let value = value * 2f64.powi(exponent.parse().unwrap());
    if negative { -value } else { value }
}

fn command(name: &str, args: &[u16]) -> Option<Command> {
    Some(match name {
        "start" => Command::Start { burn: args[0] as u8 },
        "set_burn" => Command::SetBurn(args[0] as u8),
        "pause" => Command::Pause,
        "reset" => Command::Reset,
        "home" => Command::Home,
        "center" => Command::Center,
        "box" => Command::Box,
        "forward" => Command::Forward,
        "backward" => Command::Backward,
        "redo" => Command::Redo,
        "erase" => Command::Erase,
        "power_on" => Command::PowerOn,
        "power_off" => Command::PowerOff,
        "move_to" => Command::MoveTo { x: args[0], y: args[1] },
        _ => return None,
    })
}

fn event_text(event: &Event) -> String {
    match *event {
        Event::Connected { hw } => format!("connected:{}", hw.unwrap_or(0)),
        Event::Progress { x, y } => format!("progress:{x},{y}"),
        Event::Finished => "finished".into(),
        Event::NoPower => "no_power".into(),
        Event::PowerOk => "power_ok".into(),
        Event::EraseDone => "erase_done".into(),
        Event::TransferImage => "TransferImage".into(),
        Event::Version(v) => format!("version:{v}"),
        Event::LowBatterySleep => "low_battery_sleep".into(),
        Event::TimeoutSleep => "timeout_sleep".into(),
        Event::Debug(v) => format!("debug:{v}"),
        Event::Battery(v) => format!("battery:{v}"),
        Event::BurnValue(v) => format!("burn_value:{v}"),
        Event::VerifyOk => "verify_ok".into(),
        Event::Unknown(b) => format!("unknown:{b}"),
        Event::Disconnected => "disconnected".into(),
    }
}

/// Recomputes one manifest line; returns what the Rust library produced for its last field.
fn actual(f: &[&str]) -> String {
    match f[0] {
        "prepare" => sha(base_of(f[1], f[2].parse().unwrap(), mode(f[3])).as_raw()),
        "payload" => {
            let p = protocol(f[2], f[3]);
            sha(&p.build_payload(&base_of(f[1], p.base_size(), mode(f[4]))))
        }
        "resize" => {
            let out = imaging::to_equal_small(&imaging::load(Path::new(f[1])).unwrap(), f[2].parse().unwrap());
            assert_eq!(
                (out.width().to_string().as_str(), out.height().to_string().as_str()),
                (f[3], f[4]),
                "size of {}",
                f[1]
            );
            sha(out.as_raw())
        }
        "decode" => sha(imaging::load(Path::new(f[1])).unwrap().as_raw()),
        "ops" => {
            let mut base = base_of(f[1], 490, mode(f[2]));
            for _ in 0..f[3].parse::<u32>().unwrap() {
                base = imaging::rotate90(&base);
            }
            for _ in 0..f[4].parse::<u32>().unwrap() {
                base = imaging::shrink(&base);
            }
            if !f[5].is_empty() {
                let stamp = imaging::prepare(&imaging::load(Path::new(f[5])).unwrap(), 200, Mode::Bw);
                base = imaging::place(&base, &stamp, f[6].parse().unwrap(), f[7].parse().unwrap());
            }
            sha(base.as_raw())
        }
        "gcode" => {
            let text = std::fs::read_to_string(f[1]).unwrap();
            sha(neje::gcode::rasterize(&text, f[2].parse().unwrap(), f[3] == "1", f[4].parse().unwrap()).as_raw())
        }
        "segments" => {
            let segs = neje::gcode::parse_segments(&std::fs::read_to_string(f[1]).unwrap());
            assert_eq!(segs.len().to_string(), f[2], "number of segments of {}", f[1]);
            let packed: Vec<u8> = segs.iter().flat_map(|s| [s.0, s.1, s.2, s.3]).flat_map(f64::to_le_bytes).collect();
            sha(&packed)
        }
        "hypot" => {
            let h = neje::gcode::py_hypot(float_from_hex(f[1]), float_from_hex(f[2]));
            if h == float_from_hex(f[3]) { f[3].to_owned() } else { format!("{h:e}") }
        }
        "command" => {
            let p = protocol(f[1], f[2]);
            let args: Vec<&str> = f[4].split(',').filter(|a| !a.is_empty()).collect();
            let encoded = match f[3] {
                "handshake" => Ok(p.handshake().to_vec()),
                "move" => p.encode(Command::Move(match args[0] {
                    "up" => Direction::Up,
                    "down" => Direction::Down,
                    "left" => Direction::Left,
                    _ => Direction::Right,
                })),
                name => {
                    let numbers: Vec<u16> = args.iter().map(|a| a.parse().unwrap()).collect();
                    p.encode(command(name, &numbers).unwrap_or_else(|| panic!("unknown command {name}")))
                }
            };
            match encoded {
                Ok(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
                Err(ProtocolError::Unsupported { .. }) => "none".into(),
                Err(_) => "error".into(),
            }
        }
        "parse" => {
            let stream = unhex(f[3]);
            let mut parser = Parser::new(protocol(f[1], f[2]));
            let mut events = Vec::new();
            let mut start = 0;
            let cuts = f[4].split(',').filter(|c| !c.is_empty()).map(|c| c.parse::<usize>().unwrap());
            for cut in cuts.chain([stream.len()]) {
                events.extend(parser.feed(&stream[start..cut]));
                start = cut;
            }
            events.iter().map(event_text).collect::<Vec<_>>().join(";")
        }
        "recognise" => {
            let version: Version = f[1].parse().unwrap();
            version.recognise(&unhex(f[2])).map_or("none".into(), |p| p.hw.unwrap_or(0).to_string())
        }
        other => panic!("unknown case kind {other}"),
    }
}

#[test]
#[ignore = "needs the reference data from tests/parity/reference.py (set NEJE_PARITY_DIR)"]
fn same_bytes_as_the_python_prototype() {
    let dir = std::env::var("NEJE_PARITY_DIR").expect("set NEJE_PARITY_DIR to the folder written by reference.py");
    let manifest = std::fs::read_to_string(Path::new(&dir).join("manifest.tsv")).expect("manifest.tsv");
    let lines: Vec<&str> = manifest.lines().filter(|l| !l.is_empty()).collect();
    // kind -> (cases, mismatches)
    let totals: Mutex<BTreeMap<String, (usize, Vec<String>)>> = Mutex::new(BTreeMap::new());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|scope| {
        for chunk in lines.chunks(lines.len().div_ceil(workers).max(1)) {
            let totals = &totals;
            scope.spawn(move || {
                for line in chunk {
                    let fields: Vec<&str> = line.split('\t').collect();
                    let expected = fields[fields.len() - 1];
                    let got = actual(&fields);
                    // JPEG decoders are allowed to differ (see README); they are counted apart.
                    let lossy = fields[0] == "decode" && fields[1].to_ascii_lowercase().ends_with(".jpg");
                    let kind = if lossy { "decode (JPEG)".to_owned() } else { fields[0].to_owned() };
                    let mut totals = totals.lock().unwrap();
                    let entry = totals.entry(kind).or_default();
                    entry.0 += 1;
                    if got != expected {
                        entry.1.push(format!(
                            "{}\n      expected {expected}\n      got      {got}",
                            fields[..fields.len() - 1].join(" ")
                        ));
                    }
                }
            });
        }
    });
    let totals = totals.into_inner().unwrap();
    let mut failed = false;
    for (kind, (cases, mismatches)) in &totals {
        println!("{kind:14} {:6} cases, {:5} different", cases, mismatches.len());
        for m in mismatches.iter().take(5) {
            println!("    {m}");
        }
        failed |= !mismatches.is_empty() && kind != "decode (JPEG)";
    }
    assert!(!failed, "the Rust library and the Python prototype disagree");
}
