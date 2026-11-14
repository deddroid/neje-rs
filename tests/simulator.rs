//! End-to-end tests against the virtual engraver (no machine needed).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command as Process;
use std::time::Duration;

use image::{GrayImage, Luma};
use neje::device::{self, Engraver};
use neje::protocol::{Command, Direction, Event, Version};
use neje::simulator::{Options, Simulator};

/// Asymmetric image: a black rectangle near the top left corner, to check the orientation.
fn marker() -> GrayImage {
    GrayImage::from_fn(490, 490, |x, y| Luma([if (10..60).contains(&x) && (10..40).contains(&y) { 0 } else { 255 }]))
}

fn temp_file(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn simulator(version: Version, erase_ms: u64) -> Simulator {
    Simulator::start(Options { version, erase_time: Duration::from_millis(erase_ms), ..Options::default() }).unwrap()
}

fn protocol_arg(version: Version) -> &'static str {
    // V30 goes through the full detection order, as the original launcher would.
    if version == Version::V10 { "v10" } else { "auto" }
}

fn neje(sim: &Simulator, args: &[&str]) -> std::process::Output {
    Process::new(env!("CARGO_BIN_EXE_neje")).args(["--port", sim.port(), "--lang", "en"]).args(args).output().unwrap()
}

fn engrave_with_the_cli(version: Version, erase_wait: &str) {
    let sim = simulator(version, 500);
    let source = temp_file(&format!("marker-{version}.png"));
    let sent = temp_file(&format!("sent-{version}.png"));
    marker().save(&source).unwrap();
    let out = neje(
        &sim,
        &[
            "engrave",
            source.to_str().unwrap(),
            "--burn",
            "45",
            "--save-preview",
            sent.to_str().unwrap(),
            "--erase-wait",
            erase_wait,
            "--timeout",
            "20",
            "--yes",
            "--protocol",
            protocol_arg(version),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "neje failed: {stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("Engraving finished"), "no end message: {stdout}");
    let report = sim.report();
    assert_eq!(report.burn, Some(45));
    assert_eq!(report.started, 1);
    // A V30 machine sees the V10 handshake (F6) during detection and ignores it.
    let errors: Vec<_> = report.errors.iter().filter(|e| !e.contains("0xf6")).collect();
    assert!(errors.is_empty(), "protocol violations: {errors:?}");
    let sent = image::open(&sent).unwrap().into_luma8();
    let received = image::imageops::crop_imm(&report.image.unwrap(), 0, 0, 490, 490).to_image();
    assert_eq!(sent, received, "the engraver received a different image");
    assert_eq!(sent, marker());
}

#[test]
fn engrave_v10() {
    engrave_with_the_cli(Version::V10, "0.8");
}

#[test]
fn engrave_v30() {
    engrave_with_the_cli(Version::V30, "3");
}

#[test]
fn image_sent_during_the_erase_is_reported() {
    let sim = simulator(Version::V10, 1500);
    let source = temp_file("marker-early.png");
    marker().save(&source).unwrap();
    let out = neje(&sim, &["upload", source.to_str().unwrap(), "--erase-wait", "0.1"]);
    assert!(out.status.success());
    std::thread::sleep(Duration::from_millis(300));
    assert!(sim.report().errors.iter().any(|e| e.contains("DURING")), "the early image went unnoticed");
}

#[test]
fn detection_sends_only_the_handshakes() {
    let v10 = simulator(Version::V10, 100);
    let found = device::probe(v10.port(), &Version::DETECTION_ORDER).unwrap().unwrap();
    assert_eq!(found.version, Version::V10);
    assert_eq!(v10.report().received, [vec![0xF6]], "a V10 machine must only see F6");

    let v30 = simulator(Version::V30, 100);
    let found = device::probe(v30.port(), &Version::DETECTION_ORDER).unwrap().unwrap();
    assert_eq!((found.version, found.hw), (Version::V30, Some(11)));
    assert_eq!(v30.report().received, [vec![0xFF, 9, 0, 0]], "the shared 4-byte handshake is sent once");

    let forced = simulator(Version::V30, 100);
    device::probe(forced.port(), &[Version::V30]).unwrap().unwrap();
    assert!(forced.report().errors.is_empty(), "a forced protocol must not send the V10 handshake");
    assert_eq!(device::probe(forced.port(), &[Version::V10]).unwrap(), None);
}

#[test]
fn session_commands_and_events() {
    let sim = simulator(Version::V10, 200);
    let engraver = Engraver::connect(Some(sim.port()), &[Version::V10], None).unwrap();
    assert!(engraver.acknowledged());
    assert_eq!(engraver.protocol().version, Version::V10);
    engraver.send(Command::Move(Direction::Left)).unwrap();
    engraver.send(Command::Center).unwrap();
    engraver.send(Command::SetBurn(77)).unwrap();
    assert!(engraver.send(Command::PowerOn).is_err(), "V10 has no power command");
    let mut blocks = 0;
    engraver.upload(&marker(), Some(Duration::from_millis(400)), |_, _| blocks += 1).unwrap();
    assert_eq!(blocks, 33, "32830 bytes in blocks of 1024");
    engraver.start(30).unwrap();
    let mut points = 0;
    let end = engraver.wait_for(
        |e| *e == Event::Finished,
        Some(Duration::from_secs(10)),
        |e| points += usize::from(matches!(e, Event::Progress { .. })),
        || true,
    );
    assert_eq!(end, Some(Event::Finished));
    assert!(points > 10, "only {points} positions reported");
    let report = sim.report();
    assert_eq!(report.moves, ["f503", "fb"]);
    assert_eq!(report.burn, Some(30));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn laser_commands_need_confirmation() {
    let sim = simulator(Version::V10, 100);
    // No terminal and no --yes: the command must refuse before touching the port.
    let out = neje(&sim, &["box"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    assert!(sim.report().received.is_empty());
    assert!(neje(&sim, &["box", "--yes"]).status.success());
    assert!(sim.report().moves.contains(&"f4".to_owned()));
}

#[test]
fn engrave_needs_something_to_send() {
    let sim = simulator(Version::V10, 100);
    // Without an image this would burn whatever is left in the engraver's memory.
    let out = neje(&sim, &["engrave", "--yes"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("neje start"));
    assert_eq!(sim.report().started, 0);
}

#[test]
fn pause_gets_through_without_a_handshake_reply() {
    // A V30 simulator never answers the V10 handshake: like a firmware too busy to reply.
    let sim = simulator(Version::V30, 100);
    let out = neje(&sim, &["center"]);
    assert!(!out.status.success(), "ordinary commands need an acknowledged session");
    let out = neje(&sim, &["pause"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Warning"));
}
