//! The thread that owns the serial session, so the window never waits for the port.
//!
//! The window sends [`Request`]s and reads [`Update`]s; every update wakes the window up.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use eframe::egui;
use image::GrayImage;
use neje::device::Engraver;
use neje::protocol::{Command, Event, Protocol, Version};

pub enum Request {
    Connect {
        port: Option<String>,
        versions: Vec<Version>,
    },
    Disconnect,
    Send(Command),
    /// Start engraving with this burn time.
    Start(u8),
    Upload(GrayImage),
}

pub enum Update {
    Connected {
        port: String,
        protocol: Protocol,
    },
    /// The session ended: on request, or because the port went away.
    Disconnected,
    Event(Event),
    Sent(Command),
    Started,
    UploadProgress {
        sent: usize,
        total: usize,
    },
    UploadDone,
    /// A request failed; the text is ready to show.
    Error(String),
}

pub struct Worker {
    requests: Sender<Request>,
    pub updates: Receiver<Update>,
}

impl Worker {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (requests, rx) = mpsc::channel();
        let (tx, updates) = mpsc::channel();
        thread::spawn(move || run(&rx, &Notifier { tx, ctx }));
        Worker { requests, updates }
    }

    pub fn request(&self, request: Request) {
        // The worker only stops when the window closes, and then nobody is left to tell.
        let _ = self.requests.send(request);
    }
}

struct Notifier {
    tx: Sender<Update>,
    ctx: egui::Context,
}

impl Notifier {
    fn send(&self, update: Update) {
        // A closed channel means the window is gone: the worker stops at its next request.
        let _ = self.tx.send(update);
        self.ctx.request_repaint();
    }
}

fn run(requests: &Receiver<Request>, notify: &Notifier) {
    let mut engraver: Option<Engraver> = None;
    loop {
        match requests.recv_timeout(Duration::from_millis(40)) {
            Ok(request) => handle(request, &mut engraver, notify),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some(session) = &engraver else { continue };
        let events: Vec<Event> = session.events().try_iter().collect();
        for event in events {
            let lost = event == Event::Disconnected;
            notify.send(Update::Event(event));
            if lost {
                engraver = None;
                notify.send(Update::Disconnected);
                break;
            }
        }
    }
}

fn handle(request: Request, engraver: &mut Option<Engraver>, notify: &Notifier) {
    let result = match (request, engraver.as_ref()) {
        (Request::Connect { port, versions }, _) => {
            *engraver = None; // close the old session before opening the port again
            Engraver::connect(port.as_deref(), &versions, None).map(|session| {
                notify.send(Update::Connected { port: session.port().to_owned(), protocol: session.protocol() });
                *engraver = Some(session);
            })
        }
        (Request::Disconnect, _) => {
            *engraver = None;
            notify.send(Update::Disconnected);
            Ok(())
        }
        // The window disables these while disconnected; a late click is simply dropped.
        (_, None) => Ok(()),
        (Request::Send(command), Some(session)) => session.send(command).map(|()| notify.send(Update::Sent(command))),
        (Request::Start(burn), Some(session)) => session.start(burn).map(|()| notify.send(Update::Started)),
        (Request::Upload(image), Some(session)) => session
            .upload(&image, None, |sent, total| notify.send(Update::UploadProgress { sent, total }))
            .map(|()| notify.send(Update::UploadDone)),
    };
    if let Err(e) = result {
        notify.send(Update::Error(e.to_string()));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use neje::simulator::{Options, Simulator};

    /// The next update that is not a position report, or a panic after ten seconds.
    fn next(worker: &Worker) -> Update {
        loop {
            match worker.updates.recv_timeout(Duration::from_secs(10)).expect("the worker went silent") {
                Update::Event(Event::Progress { .. }) | Update::UploadProgress { .. } => {}
                update => return update,
            }
        }
    }

    #[test]
    fn a_whole_job_through_the_worker() {
        let sim = Simulator::start(Options { erase_time: Duration::from_millis(200), ..Options::default() }).unwrap();
        let worker = Worker::spawn(egui::Context::default());

        worker.request(Request::Connect { port: Some(sim.port().to_owned()), versions: vec![Version::V10] });
        assert!(matches!(next(&worker), Update::Connected { protocol, .. } if protocol.version == Version::V10));

        let image = GrayImage::from_fn(490, 490, |x, y| image::Luma([if x < 80 && y < 40 { 0 } else { 255 }]));
        worker.request(Request::Upload(image.clone()));
        worker.request(Request::Start(55));
        let mut seen = Vec::new();
        loop {
            match next(&worker) {
                Update::UploadDone => seen.push("uploaded"),
                Update::Started => seen.push("started"),
                Update::Event(Event::Finished) => break,
                Update::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        assert_eq!(seen, ["uploaded", "started"]);
        let report = sim.report();
        assert_eq!(report.burn, Some(55));
        assert_eq!(image::imageops::crop_imm(&report.image.unwrap(), 0, 0, 490, 490).to_image(), image);
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        worker.request(Request::Send(Command::PowerOn));
        assert!(matches!(next(&worker), Update::Error(_)), "V10 has no power command");
        worker.request(Request::Disconnect);
        assert!(matches!(next(&worker), Update::Disconnected));
    }

    #[test]
    fn a_missing_engraver_is_an_error_not_a_hang() {
        let worker = Worker::spawn(egui::Context::default());
        worker.request(Request::Connect {
            port: Some("/dev/neje-does-not-exist".to_owned()),
            versions: vec![Version::V10],
        });
        assert!(matches!(next(&worker), Update::Error(_)));
    }
}
