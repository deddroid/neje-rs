//! The thread that owns the serial session, so the window never waits for the port.
//!
//! The window sends [`Request`]s and reads [`Update`]s; every update wakes the window up.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use eframe::egui;
use image::GrayImage;
use neje::device::{AFTER_UPLOAD_PAUSE, DeviceError, Engraver};
use neje::protocol::{Command, Event, Protocol, Version};

pub enum Request {
    Connect {
        port: Option<String>,
        versions: Vec<Version>,
    },
    Disconnect,
    Send(Command),
    /// Start (or resume) engraving with this burn time.
    Start(u8),
    Upload(GrayImage),
    /// Send and engrave these images one after the other, without restarting the engraver
    /// in between.
    RunJobs {
        images: Vec<GrayImage>,
        burn: u8,
    },
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
    /// Job `index` (from 1) of `total` is about to be sent.
    Job {
        index: usize,
        total: usize,
    },
    /// The last job of a sequence has finished.
    JobsDone,
    /// The board restarted while engraving (it says `79`): the job is incomplete and its
    /// end message will never come.
    Restarted,
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
        thread::spawn(move || {
            let mut state = State { engraver: None, queue: VecDeque::new(), total: 0, burn: 0, engraving: false };
            state.run(&rx, &Notifier { tx, ctx });
        });
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

struct State {
    engraver: Option<Engraver>,
    /// Jobs of the running sequence still to be sent.
    queue: VecDeque<GrayImage>,
    /// Number of jobs of the running sequence.
    total: usize,
    burn: u8,
    /// True between a start and its end message.
    engraving: bool,
}

impl State {
    fn run(&mut self, requests: &Receiver<Request>, notify: &Notifier) {
        loop {
            match requests.recv_timeout(Duration::from_millis(40)) {
                Ok(request) => {
                    if let Err(e) = self.handle(request, notify) {
                        self.abandon_jobs();
                        notify.send(Update::Error(e.to_string()));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let events: Vec<Event> = self.engraver.iter().flat_map(|session| session.events().try_iter()).collect();
            for event in events {
                notify.send(Update::Event(event));
                if let Err(e) = self.on_event(event, notify) {
                    self.abandon_jobs();
                    notify.send(Update::Error(e.to_string()));
                }
            }
        }
    }

    fn abandon_jobs(&mut self) {
        self.queue.clear();
        self.total = 0;
    }

    fn close(&mut self, notify: &Notifier) {
        self.engraver = None;
        self.engraving = false;
        self.abandon_jobs();
        notify.send(Update::Disconnected);
    }

    fn on_event(&mut self, event: Event, notify: &Notifier) -> Result<(), DeviceError> {
        match event {
            Event::Disconnected => self.close(notify),
            Event::Finished => {
                self.engraving = false;
                if !self.queue.is_empty() {
                    self.next_job(notify)?;
                } else if self.total != 0 {
                    self.total = 0;
                    notify.send(Update::JobsDone);
                }
            }
            Event::PowerOk if self.engraving => {
                self.engraving = false;
                self.abandon_jobs();
                notify.send(Update::Restarted);
            }
            _ => {}
        }
        Ok(())
    }

    /// Sends the next image of the sequence and starts engraving it.
    fn next_job(&mut self, notify: &Notifier) -> Result<(), DeviceError> {
        let (Some(session), Some(image)) = (&self.engraver, self.queue.pop_front()) else {
            return Ok(());
        };
        notify.send(Update::Job { index: self.total - self.queue.len(), total: self.total });
        session.upload(&image, None, |sent, total| notify.send(Update::UploadProgress { sent, total }))?;
        notify.send(Update::UploadDone);
        thread::sleep(AFTER_UPLOAD_PAUSE);
        session.start(self.burn)?;
        self.engraving = true;
        notify.send(Update::Started);
        Ok(())
    }

    fn handle(&mut self, request: Request, notify: &Notifier) -> Result<(), DeviceError> {
        match request {
            Request::Connect { port, versions } => {
                self.engraver = None; // close the old session before opening the port again
                let session = Engraver::connect(port.as_deref(), &versions, None)?;
                notify.send(Update::Connected { port: session.port().to_owned(), protocol: session.protocol() });
                self.engraver = Some(session);
            }
            Request::Disconnect => self.close(notify),
            request => {
                // The window disables these while disconnected; a late click is simply dropped.
                let Some(session) = &self.engraver else { return Ok(()) };
                match request {
                    Request::Send(command) => {
                        session.send(command)?;
                        if command == Command::Reset {
                            // The restart loses the position: the remaining jobs would not line up.
                            self.engraving = false;
                            self.abandon_jobs();
                        }
                        notify.send(Update::Sent(command));
                    }
                    Request::Start(burn) => {
                        session.start(burn)?;
                        self.engraving = true;
                        notify.send(Update::Started);
                    }
                    Request::Upload(image) => {
                        session
                            .upload(&image, None, |sent, total| notify.send(Update::UploadProgress { sent, total }))?;
                        notify.send(Update::UploadDone);
                    }
                    Request::RunJobs { images, burn } => {
                        self.total = images.len();
                        self.queue = images.into();
                        self.burn = burn;
                        self.next_job(notify)?;
                    }
                    Request::Connect { .. } | Request::Disconnect => unreachable!("handled above"),
                }
            }
        }
        Ok(())
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

    #[test]
    fn jobs_run_one_after_the_other() {
        let sim = Simulator::start(Options { erase_time: Duration::from_millis(200), ..Options::default() }).unwrap();
        let worker = Worker::spawn(egui::Context::default());
        worker.request(Request::Connect { port: Some(sim.port().to_owned()), versions: vec![Version::V10] });
        assert!(matches!(next(&worker), Update::Connected { .. }));

        let band = |x0: u32| {
            GrayImage::from_fn(490, 490, move |x, y| image::Luma([if x / 20 == x0 && y < 30 { 0 } else { 255 }]))
        };
        worker.request(Request::RunJobs { images: vec![band(2), band(9)], burn: 40 });
        let mut seen = Vec::new();
        loop {
            match next(&worker) {
                Update::Job { index, total } => seen.push(format!("job {index}/{total}")),
                Update::Started => seen.push("started".to_owned()),
                Update::JobsDone => break,
                Update::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        assert_eq!(seen, ["job 1/2", "started", "job 2/2", "started"]);
        let report = sim.report();
        assert_eq!(
            report.jobs,
            [band(2), band(9)].map(|b| neje::bitmap::unpack_1bpp(
                &neje::protocol::Protocol::new(Version::V10, None).build_payload(&b)[62..],
                512,
                512,
                0
            ))
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }

    #[test]
    fn a_restart_stops_the_sequence() {
        let options = Options { erase_time: Duration::from_millis(200), restart_after: Some(20), ..Options::default() };
        let sim = Simulator::start(options).unwrap();
        let worker = Worker::spawn(egui::Context::default());
        worker.request(Request::Connect { port: Some(sim.port().to_owned()), versions: vec![Version::V10] });
        assert!(matches!(next(&worker), Update::Connected { .. }));
        let image = GrayImage::from_fn(490, 490, |x, y| image::Luma([if x < 80 && y < 40 { 0 } else { 255 }]));
        worker.request(Request::RunJobs { images: vec![image.clone(), image], burn: 40 });
        loop {
            match next(&worker) {
                Update::Restarted => break,
                Update::JobsDone => panic!("the sequence must not complete after a restart"),
                Update::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(sim.report().jobs.len(), 1, "the second job must not be sent");
    }
}
