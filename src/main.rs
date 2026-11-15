//! Command line: `neje <command> ...`.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use neje::compose::{Job, TextLayer};
use neje::device::{self, Engraver};
use neje::doctor;
use neje::i18n::{self, Lang, tr};
use neje::imaging::{self, Mode};
use neje::messages::{
    describe, laser_checklist, mode_description, mode_name, problem_fix_label, problem_text, problem_title,
};
use neje::plan::{self, Plan, PrintMode};
use neje::protocol::{BURN_DEFAULT, BURN_MAX, BURN_MIN, Command, Direction, Event, Protocol, Version};

#[derive(Parser)]
#[command(name = "neje", version, about = "Control NEJE DK-8-KZ laser engravers (protocols V10/V20/V21/V30)")]
struct Cli {
    /// Serial port (default: search the CH340 converters)
    #[arg(short, long, global = true)]
    port: Option<String>,
    /// Protocol of the engraver. "auto" tries them all like the original launcher: use it
    /// only for the newer models, a busy V10 firmware reads their handshake as a burn time
    #[arg(long, global = true, value_enum, default_value = "v10", ignore_case = true)]
    protocol: ProtocolArg,
    /// Hardware version (V30 only: 11, 12, 13)
    #[arg(long, global = true)]
    hw: Option<u8>,
    /// Language of the messages (default: from the locale)
    #[arg(long, global = true, value_enum)]
    lang: Option<LangArg>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum LangArg {
    En,
    It,
}

#[derive(Clone, Copy, ValueEnum)]
enum ProtocolArg {
    V10,
    V20,
    V21,
    V30,
    Auto,
}

impl ProtocolArg {
    fn versions(self) -> &'static [Version] {
        match self {
            ProtocolArg::V10 => &[Version::V10],
            ProtocolArg::V20 => &[Version::V20],
            ProtocolArg::V21 => &[Version::V21],
            ProtocolArg::V30 => &[Version::V30],
            ProtocolArg::Auto => &Version::DETECTION_ORDER,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    /// Black and white with the Otsu threshold (default of the original program)
    Bw,
    /// NEJE dithering, for photos
    Dither,
    /// Fixed threshold at 127
    GrayThreshold,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum PrintModeArg {
    /// One upload, one start
    Single,
    /// Vertical bands engraved one after the other, to skip the empty space between distant parts
    Split,
    /// Centre lines with a stroke of a few pixels instead of filled shapes
    Line,
}

#[derive(Clone, Copy, ValueEnum)]
enum DirectionArg {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, ValueEnum)]
enum SimProtocol {
    V10,
    V30,
}

#[derive(Args)]
struct ImageArgs {
    /// Picture to engrave (JPEG, PNG, BMP, GIF, TIFF, WebP)
    image: Option<PathBuf>,
    /// How the picture becomes black and white
    #[arg(long, value_enum, default_value = "bw")]
    mode: ModeArg,
    /// Text to add (\n starts a new line)
    #[arg(long)]
    text: Option<String>,
    /// Font file (.ttf/.otf) or file name in the system font folders (default: the
    /// built-in Inter)
    #[arg(long)]
    font: Option<String>,
    /// Text size in pixels
    #[arg(long, default_value_t = 48)]
    size: u32,
    /// Centre of the text in pixels (default: centre of the area)
    #[arg(long, num_args = 2, value_names = ["X", "Y"], allow_negative_numbers = true)]
    at: Option<Vec<i64>>,
    /// Number of 90° clockwise rotations
    #[arg(long, default_value_t = 0)]
    rotate: u32,
    /// Number of reductions to 90% (like the button of the original program)
    #[arg(long, default_value_t = 0)]
    shrink: u32,
    /// G-code file to rasterise
    #[arg(long)]
    gcode: Option<PathBuf>,
    /// G-code: scale the drawing to fill the work area
    #[arg(long)]
    fit: bool,
    /// G-code: line thickness in pixels
    #[arg(long, default_value_t = 1)]
    line: u32,
    /// How the image is sent to the engraver ("neje preview" compares the modes)
    #[arg(long, value_enum, default_value = "single")]
    print_mode: PrintModeArg,
    /// Line mode: stroke width in pixels
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=i64::from(plan::MAX_STROKE)))]
    stroke: u8,
}

impl ImageArgs {
    fn print_mode(&self) -> PrintMode {
        match self.print_mode {
            PrintModeArg::Single => PrintMode::Single,
            PrintModeArg::Split => PrintMode::Split,
            PrintModeArg::Line => PrintMode::Line { stroke: self.stroke },
        }
    }

    fn job(&self) -> Job {
        Job {
            image: self.image.clone(),
            mode: match self.mode {
                ModeArg::Bw => Mode::Bw,
                ModeArg::Dither => Mode::Dither,
                ModeArg::GrayThreshold => Mode::GrayThreshold,
            },
            gcode: self.gcode.clone(),
            gcode_fit: self.fit,
            gcode_line_px: self.line,
            rotate: self.rotate,
            shrink: self.shrink,
            text: self.text.clone().map(|text| TextLayer {
                text,
                font: self.font.clone(),
                size: self.size,
                at: self.at.as_ref().map(|at| (at[0], at[1])),
            }),
        }
    }
}

#[derive(Args)]
struct UploadArgs {
    #[command(flatten)]
    image: ImageArgs,
    /// Save the image that is sent
    #[arg(long)]
    save_preview: Option<PathBuf>,
    /// Seconds to wait after the erase command (default: 3.5 on V10)
    #[arg(long)]
    erase_wait: Option<f64>,
}

#[derive(Args)]
struct RunArgs {
    /// Burn time, 1-120
    #[arg(short, long, default_value_t = BURN_DEFAULT, value_parser = clap::value_parser!(u8).range(i64::from(BURN_MIN)..=i64::from(BURN_MAX)))]
    burn: u8,
    /// Give up waiting for the end after this many seconds
    #[arg(long)]
    timeout: Option<f64>,
    #[command(flatten)]
    laser: LaserArgs,
}

#[derive(Args)]
struct LaserArgs {
    /// Do not ask for the laser safety confirmation
    #[arg(short, long)]
    yes: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the serial ports
    Ports,
    /// Find the engraver and its protocol
    Detect,
    /// Build the image that would be engraved (no machine needed)
    Preview {
        #[command(flatten)]
        image: ImageArgs,
        #[arg(short, long, default_value = "preview.png")]
        output: PathBuf,
        /// Also save the exact bytes that would be sent (first job)
        #[arg(long)]
        payload: Option<PathBuf>,
        /// Burn time used for the time estimates, 1-120
        #[arg(short, long, default_value_t = BURN_DEFAULT, value_parser = clap::value_parser!(u8).range(i64::from(BURN_MIN)..=i64::from(BURN_MAX)))]
        burn: u8,
        /// Save a picture of the plan: black = burnt, orange = empty space crossed,
        /// green = empty space no longer crossed, blue = cut between two jobs
        #[arg(long)]
        plan_image: Option<PathBuf>,
        /// Save the image of every job as PREFIX-1.png, PREFIX-2.png, ...
        #[arg(long, value_name = "PREFIX")]
        jobs: Option<String>,
    },
    /// Send the image to the engraver
    Upload(UploadArgs),
    /// Send the image and start engraving, one job after the other when the print mode
    /// splits it. SWITCHES THE LASER ON
    Engrave {
        #[command(flatten)]
        upload: UploadArgs,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Start engraving the image already in the engraver's memory, whatever it is (it
    /// survives power off). SWITCHES THE LASER ON
    Start(RunArgs),
    /// Pause the engraving. With --port it is sent even if the engraver does not answer
    Pause,
    /// Restart the engraver (Reboot): it finds its edges again, so the position is lost.
    /// With --port it is sent even if the engraver does not answer
    Reset,
    /// Go to the origin of the work area (V10). Ignored after a finished job, until "reset"
    Home,
    /// Go to the centre of the image in memory; also stops "box". Ignored after a finished
    /// job, until "reset"
    Center,
    /// Trace the outline of the image over and over, until "center". SWITCHES THE LASER ON
    Box(LaserArgs),
    /// Skip forward in the engraving. SWITCHES THE LASER ON
    Forward(LaserArgs),
    /// Go back in the engraving. SWITCHES THE LASER ON
    Backward(LaserArgs),
    /// Start again from the beginning (Redo). SWITCHES THE LASER ON
    Redo(LaserArgs),
    /// Power on (V2x/V30)
    PowerOn,
    /// Power off (V2x/V30)
    PowerOff,
    /// Shift where the image will be engraved, 8 pixels per step (on the DK-8-KZ "down"
    /// often stalls)
    Move {
        #[arg(value_enum)]
        direction: DirectionArg,
        /// Number of steps, at most 20 per call: the machine has no limit switches and
        /// keeps pushing against the end stop
        #[arg(default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=20))]
        steps: u32,
        /// Seconds between steps
        #[arg(long, default_value_t = 0.1)]
        delay: f64,
    },
    /// Set the burn time (1-120)
    SetBurn {
        #[arg(value_parser = clap::value_parser!(u8).range(i64::from(BURN_MIN)..=i64::from(BURN_MAX)))]
        value: u8,
    },
    /// Show the messages from the engraver
    Monitor {
        /// Only listen: do not send the handshake (needs --port)
        #[arg(long)]
        passive: bool,
    },
    /// Run a virtual engraver for tests (Unix)
    Simulate {
        #[arg(long, value_enum, default_value = "v10")]
        sim_protocol: SimProtocol,
        /// Where to save the image the virtual engraver receives
        #[arg(short, long, default_value = "virtual-engraving.png")]
        output: PathBuf,
    },
    /// Check what the system lacks for the engraver (driver, permission on the port)
    Doctor {
        /// Solve what can be solved; asks for the administrator password
        #[arg(long)]
        fix: bool,
    },
    /// Open the graphical interface
    Gui,
}

struct App {
    port: Option<String>,
    protocol: ProtocolArg,
    hw: Option<u8>,
    interrupted: Arc<AtomicBool>,
}

impl App {
    /// Finds the engraver and opens a session it has acknowledged.
    fn connect(&self) -> Result<Engraver> {
        let engraver = Engraver::connect(self.port.as_deref(), self.protocol.versions(), self.hw)?;
        if !engraver.acknowledged() {
            bail!(
                "{} {}",
                tr("no reply to the handshake from", "nessuna risposta alla stretta di mano da"),
                engraver.port()
            );
        }
        Ok(engraver)
    }

    /// Session for the commands that must get through even when the firmware is too busy to
    /// answer the handshake (pause, reset): with a named port nothing is detected.
    fn connect_unchecked(&self) -> Result<Engraver> {
        let (Some(port), [version]) = (&self.port, self.protocol.versions()) else {
            return self.connect();
        };
        let engraver = Engraver::open(port, Protocol::new(*version, self.hw))?;
        if !engraver.acknowledged() {
            eprintln!(
                "{} {port}",
                tr(
                    "Warning: no reply to the handshake, sending the command anyway to",
                    "Attenzione: nessuna risposta alla stretta di mano, invio comunque il comando a"
                )
            );
        }
        Ok(engraver)
    }

    /// Work area before connecting: from the chosen protocol (V10 for "auto").
    fn offline_protocol(&self) -> Protocol {
        Protocol::new(self.protocol.versions()[0], self.hw)
    }

    fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Relaxed)
    }

    fn simple(&self, command: Command) -> Result<()> {
        let engraver = if matches!(command, Command::Pause | Command::Reset) {
            self.connect_unchecked()?
        } else {
            self.connect()?
        };
        engraver.send(command)?;
        thread::sleep(Duration::from_millis(200));
        println!("OK");
        Ok(())
    }

    fn laser(&self, command: Command, args: &LaserArgs) -> Result<()> {
        confirm_laser(args.yes)?;
        self.simple(command)
    }

    /// The image described by `args`, prepared for its print mode.
    fn plan(&self, args: &ImageArgs, protocol: Protocol, burn: u8) -> Result<Plan> {
        let base = args.job().build(protocol.base_size())?;
        Ok(Plan::new(&base, args.print_mode(), burn, protocol))
    }

    /// Erases the memory of the engraver and sends `image`.
    fn send_image(&self, engraver: &Engraver, image: &image::GrayImage, args: &UploadArgs) -> Result<()> {
        println!("{}", tr("Erasing the memory...", "Cancellazione della memoria..."));
        let wait = args.erase_wait.map(Duration::try_from_secs_f64).transpose().context("invalid --erase-wait")?;
        engraver.upload(image, wait, |sent, total| {
            print!("\r  {:3}%  ({sent}/{total} byte)", sent * 100 / total);
            // Progress output only: a failed flush is not worth stopping the upload.
            let _ = io::stdout().flush();
        })?;
        println!("\n{}", tr("Image sent", "Immagine inviata"));
        Ok(())
    }

    fn upload(&self, engraver: &Engraver, args: &UploadArgs) -> Result<()> {
        let plan = self.plan(&args.image, engraver.protocol(), BURN_DEFAULT)?;
        if plan.jobs.len() > 1 {
            bail!(
                "{} {} {}",
                tr("this print mode splits the image in", "questa modalità divide l'immagine in"),
                plan.jobs.len(),
                tr(
                    "jobs, and the engraver holds one image at a time: use \"neje engrave\"",
                    "lavori, e l'incisore tiene una sola immagine alla volta: usa \"neje engrave\"",
                )
            );
        }
        if let Some(path) = &args.save_preview {
            plan.image.save(path).with_context(|| format!("cannot save {}", path.display()))?;
        }
        self.send_image(engraver, &plan.jobs[0].image, args)
    }

    /// Sends and engraves every job of the plan, one after the other.
    fn engrave(&self, engraver: &Engraver, upload: &UploadArgs, run: &RunArgs) -> Result<()> {
        let plan = self.plan(&upload.image, engraver.protocol(), run.burn)?;
        if plan.is_empty() {
            bail!(tr("the image is empty: nothing to burn", "l'immagine è vuota: niente da bruciare"));
        }
        if let Some(path) = &upload.save_preview {
            plan.image.save(path).with_context(|| format!("cannot save {}", path.display()))?;
        }
        println!(
            "{}: {}, {} {}",
            mode_name(plan.mode),
            jobs_text(plan.jobs.len()),
            tr("estimated time", "tempo stimato"),
            plan::clock(plan.seconds)
        );
        for (i, job) in plan.jobs.iter().enumerate() {
            if plan.jobs.len() > 1 {
                println!(
                    "{} {}/{}: {} {}-{}, {} {}",
                    tr("Job", "Lavoro"),
                    i + 1,
                    plan.jobs.len(),
                    tr("columns", "colonne"),
                    job.columns.start,
                    job.columns.end - 1,
                    tr("estimated time", "tempo stimato"),
                    plan::clock(job.seconds)
                );
            }
            self.send_image(engraver, &job.image, upload)?;
            thread::sleep(device::AFTER_UPLOAD_PAUSE);
            // The engraver must not be restarted between two jobs, or they no longer line up.
            if !self.run(engraver, run)? {
                break;
            }
        }
        Ok(())
    }

    /// Starts the engraving and waits for its end; false when it did not finish (Ctrl+C,
    /// time limit, connection lost).
    fn run(&self, engraver: &Engraver, args: &RunArgs) -> Result<bool> {
        println!(
            "{} {}",
            tr("Starting the engraving, burn time", "Avvio dell'incisione, tempo di bruciatura"),
            args.burn
        );
        // Forget the messages that arrived before the start (handshake replies).
        engraver.events().try_iter().for_each(drop);
        engraver.start(args.burn)?;
        let started = Instant::now();
        let mut last_shown = Instant::now() - Duration::from_secs(2);
        let timeout = args.timeout.map(Duration::try_from_secs_f64).transpose().context("invalid --timeout")?;
        let end = engraver.wait_for(
            |event| matches!(event, Event::Finished | Event::Disconnected | Event::PowerOk),
            timeout,
            |event| match *event {
                Event::Progress { x, y } => {
                    if last_shown.elapsed() > Duration::from_secs(1) {
                        last_shown = Instant::now();
                        let secs = started.elapsed().as_secs();
                        print!("\r  X:{x:3} Y:{y:3}   {} {:02}:{:02}", tr("time", "tempo"), secs / 60, secs % 60);
                        // Progress output only.
                        let _ = io::stdout().flush();
                    }
                }
                // Bytes the original ignores too (e.g. the 0x70 after the handshake reply).
                Event::Unknown(_) => {}
                other => println!("\n{}", describe(&other)),
            },
            || !self.interrupted(),
        );
        if self.interrupted() {
            println!("\n{}", tr("Interrupted: pausing the engraver", "Interrotto: metto in pausa l'incisore"));
            engraver.send(Command::Pause)?;
            return Ok(false);
        }
        match end {
            Some(Event::Finished) => Ok(true),
            // The board answers 79 when it restarts: the end message will never come.
            Some(Event::PowerOk) => bail!(tr(
                "the engraver restarted during the job, which is incomplete. Power it from a 5 V supply, not from the USB ports of the computer",
                "l'incisore si è riavviato durante il lavoro, che è incompleto. Alimentalo con un alimentatore 5 V, non dalle porte USB del computer",
            )),
            Some(_) => Ok(false),
            None => {
                println!(
                    "\n{}",
                    tr("No end message within the time limit", "Nessun messaggio di fine entro il tempo massimo")
                );
                Ok(false)
            }
        }
    }
}

fn jobs_text(n: usize) -> String {
    if n == 1 { tr("1 job", "1 lavoro").to_owned() } else { format!("{n} {}", tr("jobs", "lavori")) }
}

/// Asks the operator to confirm the laser safety conditions, unless `--yes` was given.
fn confirm_laser(yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!(tr(
            "this command switches the laser on: pass --yes to confirm that the safety conditions are met",
            "questo comando accende il laser: passa --yes per confermare che le condizioni di sicurezza sono rispettate",
        ));
    }
    println!(
        "{}",
        tr(
            "This command SWITCHES THE LASER ON AT FULL POWER. Before continuing:",
            "Questo comando ACCENDE IL LASER A PIENA POTENZA. Prima di continuare:"
        )
    );
    for item in laser_checklist() {
        println!("  - {item};");
    }
    print!("{} ", tr("Type \"yes\" to continue:", "Scrivi \"si\" per continuare:"));
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    match answer.trim().to_lowercase().as_str() {
        "yes" | "y" | "si" | "sì" | "s" => Ok(()),
        _ => bail!(tr("cancelled", "annullato")),
    }
}

fn ports() -> Result<()> {
    let ports = device::ports()?;
    if ports.is_empty() {
        println!("{}", tr("No serial port found", "Nessuna porta seriale trovata"));
    }
    for p in ports {
        let id = p.usb.map_or("----:----".to_owned(), |(vid, pid)| format!("{vid:04X}:{pid:04X}"));
        let tag =
            if p.is_ch340() { tr("  [CH340 - probably the engraver]", "  [CH340 - probabile incisore]") } else { "" };
        println!("{}  {id}  {}{tag}", p.name, p.description);
    }
    Ok(())
}

struct PreviewOutputs<'a> {
    output: &'a PathBuf,
    payload: Option<&'a PathBuf>,
    plan_image: Option<&'a PathBuf>,
    jobs: Option<&'a str>,
}

fn preview(app: &App, image: &ImageArgs, burn: u8, out: &PreviewOutputs) -> Result<()> {
    let protocol = app.offline_protocol();
    let base = image.job().build(protocol.base_size())?;
    let chosen = Plan::new(&base, image.print_mode(), burn, protocol);
    let save = |img: &image::GrayImage, path: &std::path::Path| {
        img.save(path).with_context(|| format!("cannot save {}", path.display()))
    };
    save(&chosen.image, out.output)?;
    println!(
        "{} {} ({}x{}, {:.1}% {})",
        tr("Preview saved in", "Anteprima salvata in"),
        out.output.display(),
        chosen.image.width(),
        chosen.image.height(),
        imaging::black_ratio(&chosen.image) * 100.0,
        tr("of the pixels to burn", "di pixel da bruciare"),
    );

    // The three modes side by side, so the choice can be made on numbers.
    println!("\n{} {burn}:", tr("Estimated times at burn time", "Tempi stimati con tempo di bruciatura"));
    let stroke = if let PrintMode::Line { stroke } = chosen.mode { stroke } else { 1 };
    for mode in [PrintMode::Single, PrintMode::Split, PrintMode::Line { stroke }] {
        let plan = if mode == chosen.mode { chosen.clone() } else { Plan::new(&base, mode, burn, protocol) };
        let cost = plan.cost();
        let saved = plan.saved_seconds();
        let gain = if saved.abs() < 0.5 || plan.single_seconds <= 0.0 {
            String::new()
        } else {
            format!("  ({:+.0}%)", -100.0 * saved / plan.single_seconds)
        };
        println!(
            "{} {:<22} {:>9}  {:>7} px {}  {:>7} px {}  {:>6}{gain}",
            if mode == chosen.mode { "*" } else { " " },
            mode_name(mode),
            jobs_text(plan.jobs.len()),
            cost.burnt,
            tr("burnt", "bruciati"),
            cost.crossed,
            tr("crossed", "attraversati"),
            plan::clock(plan.seconds),
        );
    }
    println!("\n{}", mode_description(chosen.mode));

    if let Some(path) = out.plan_image {
        chosen.preview().save(path).with_context(|| format!("cannot save {}", path.display()))?;
        println!("{} {}", tr("Picture of the plan saved in", "Disegno del piano salvato in"), path.display());
    }
    if let Some(prefix) = out.jobs {
        for (i, job) in chosen.jobs.iter().enumerate() {
            let path = PathBuf::from(format!("{prefix}-{}.png", i + 1));
            save(&job.image, &path)?;
            println!(
                "{} {}: {} ({} {}-{}, {})",
                tr("Job", "Lavoro"),
                i + 1,
                path.display(),
                tr("columns", "colonne"),
                job.columns.start,
                job.columns.end.saturating_sub(1),
                plan::clock(job.seconds)
            );
        }
    }
    if let Some(path) = out.payload {
        std::fs::write(path, protocol.build_payload(&chosen.jobs[0].image))
            .with_context(|| format!("cannot save {}", path.display()))?;
        println!(
            "{} ({}) {} {}",
            tr("Bytes for the engraver", "Dati per l'incisore"),
            protocol.version,
            tr("saved in", "salvati in"),
            path.display()
        );
    }
    Ok(())
}

fn monitor(app: &App, passive: bool) -> Result<()> {
    let engraver = if passive {
        let Some(port) = &app.port else {
            bail!(tr("--passive needs --port", "--passive richiede --port"));
        };
        Engraver::listen(port, app.offline_protocol())?
    } else {
        app.connect()?
    };
    println!("{}", tr("Listening (Ctrl+C to quit)...", "In ascolto (Ctrl+C per uscire)..."));
    engraver.wait_for(
        |event| *event == Event::Disconnected,
        None,
        |event| println!("{}", describe(event)),
        || !app.interrupted(),
    );
    Ok(())
}

#[cfg(unix)]
fn simulate(app: &App, sim_protocol: SimProtocol, output: PathBuf) -> Result<()> {
    use neje::simulator::{Options, Simulator};
    let version = match sim_protocol {
        SimProtocol::V10 => Version::V10,
        SimProtocol::V30 => Version::V30,
    };
    let options = Options {
        version,
        hw: app.hw.unwrap_or(11),
        out_png: Some(output),
        // Slow enough to watch the trace, far faster than the real machine.
        pixel_delay: Duration::from_micros(300),
        ..Options::default()
    };
    let sim = Simulator::start(options).context("cannot create the pseudo-terminal")?;
    println!("{} {version}: {}", tr("Virtual engraver", "Incisore virtuale"), sim.port());
    println!("{}  neje --port {} engrave photo.jpg", tr("Try:", "Prova:"), sim.port());
    println!("{}", tr("Ctrl+C to stop", "Ctrl+C per terminare"));
    while !app.interrupted() {
        thread::sleep(Duration::from_millis(500));
        for error in sim.take_errors() {
            println!("[sim] {}: {error}", tr("ERROR", "ERRORE"));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn simulate(_app: &App, _sim_protocol: SimProtocol, _output: PathBuf) -> Result<()> {
    bail!("the virtual engraver needs a pseudo-terminal and is only available on Unix")
}

/// Lists what the system lacks and, with `fix`, solves it as root.
fn doctor(fix: bool) -> Result<()> {
    let problems = doctor::check();
    if problems.is_empty() {
        println!(
            "{}",
            tr(
                "Nothing is missing. (An engraver that is not plugged in cannot be checked.)",
                "Non manca nulla. (Un incisore non collegato non si può controllare.)"
            )
        );
        return Ok(());
    }
    for problem in &problems {
        println!("{}\n  {}", problem_title(problem), problem_text(problem));
        let Some(solution) = problem.fix() else { continue };
        if !fix {
            println!("  {}: neje doctor --fix", problem_fix_label(problem));
            continue;
        }
        let Some(elevator) = doctor::elevator(false) else {
            bail!(tr("neither sudo nor pkexec found: run as root", "né sudo né pkexec trovati: esegui come root"));
        };
        solution.run(elevator).map_err(anyhow::Error::msg)?;
        println!(
            "  {}",
            tr("Done. Unplug the engraver and plug it in again.", "Fatto. Scollega e ricollega l'incisore.")
        );
    }
    if !fix {
        bail!(tr("the engraver cannot be used yet", "l'incisore non si può ancora usare"));
    }
    Ok(())
}

/// Starts `neje-gui`, installed next to this program.
fn gui(app: &App) -> Result<()> {
    let name = if cfg!(windows) { "neje-gui.exe" } else { "neje-gui" };
    let exe = std::env::current_exe().context("cannot find this program's path")?.with_file_name(name);
    let mut command = std::process::Command::new(&exe);
    if let Some(port) = &app.port {
        command.args(["--port", port]);
    }
    if let [version] = app.protocol.versions() {
        command.args(["--protocol", version.name()]);
    }
    command.spawn().with_context(|| format!("cannot start {}", exe.display()))?;
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let interrupted = Arc::new(AtomicBool::new(false));
    ctrlc::set_handler({
        let interrupted = Arc::clone(&interrupted);
        move || {
            // A second Ctrl+C quits at once.
            if interrupted.swap(true, Ordering::Relaxed) {
                std::process::exit(130);
            }
        }
    })
    .context("cannot install the Ctrl+C handler")?;
    let app = App { port: cli.port, protocol: cli.protocol, hw: cli.hw, interrupted };
    match cli.command {
        Cmd::Ports => ports(),
        Cmd::Detect => {
            let (port, protocol) = device::detect(app.port.as_deref(), app.protocol.versions())?;
            let hw = protocol.hw.map(|hw| format!(", hardware {hw}")).unwrap_or_default();
            println!(
                "{} {port}: {} {}{hw}",
                tr("Engraver found on", "Incisore trovato su"),
                tr("protocol", "protocollo"),
                protocol.version
            );
            Ok(())
        }
        Cmd::Preview { image, output, payload, burn, plan_image, jobs } => {
            let outputs = PreviewOutputs {
                output: &output,
                payload: payload.as_ref(),
                plan_image: plan_image.as_ref(),
                jobs: jobs.as_deref(),
            };
            preview(&app, &image, burn, &outputs)
        }
        Cmd::Upload(args) => app.upload(&app.connect()?, &args),
        Cmd::Engrave { upload, run } => {
            if upload.image.job().is_empty() {
                bail!(tr(
                    "nothing to engrave: give a picture, --text or --gcode (\"neje start\" engraves the image already in the engraver's memory)",
                    "niente da incidere: indica un'immagine, --text o --gcode (\"neje start\" incide l'immagine già nella memoria dell'incisore)",
                ));
            }
            confirm_laser(run.laser.yes)?;
            let engraver = app.connect()?;
            app.engrave(&engraver, &upload, &run)
        }
        Cmd::Start(args) => {
            confirm_laser(args.laser.yes)?;
            app.run(&app.connect()?, &args).map(drop)
        }
        Cmd::Pause => app.simple(Command::Pause),
        Cmd::Reset => app.simple(Command::Reset),
        Cmd::Home => app.simple(Command::Home),
        Cmd::Center => app.simple(Command::Center),
        Cmd::Box(args) => app.laser(Command::Box, &args),
        Cmd::Forward(args) => app.laser(Command::Forward, &args),
        Cmd::Backward(args) => app.laser(Command::Backward, &args),
        Cmd::Redo(args) => app.laser(Command::Redo, &args),
        Cmd::PowerOn => app.simple(Command::PowerOn),
        Cmd::PowerOff => app.simple(Command::PowerOff),
        Cmd::Move { direction, steps, delay } => {
            let direction = match direction {
                DirectionArg::Up => Direction::Up,
                DirectionArg::Down => Direction::Down,
                DirectionArg::Left => Direction::Left,
                DirectionArg::Right => Direction::Right,
            };
            let delay = Duration::try_from_secs_f64(delay).context("invalid --delay")?;
            let engraver = app.connect()?;
            for _ in 0..steps {
                engraver.send(Command::Move(direction))?;
                thread::sleep(delay);
            }
            println!("OK");
            Ok(())
        }
        Cmd::SetBurn { value } => app.simple(Command::SetBurn(value)),
        Cmd::Monitor { passive } => monitor(&app, passive),
        Cmd::Simulate { sim_protocol, output } => simulate(&app, sim_protocol, output),
        Cmd::Doctor { fix } => doctor(fix),
        Cmd::Gui => gui(&app),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    i18n::set(match cli.lang {
        Some(LangArg::En) => Lang::En,
        Some(LangArg::It) => Lang::It,
        None => Lang::from_env(),
    });
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}: {e:#}", tr("Error", "Errore"));
            ExitCode::FAILURE
        }
    }
}
