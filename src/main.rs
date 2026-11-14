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
use neje::i18n::{self, Lang, tr};
use neje::imaging::{self, Mode};
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
    /// Font file (.ttf/.otf) or file name in the system font folders
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
}

impl ImageArgs {
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
        /// Also save the exact bytes that would be sent
        #[arg(long)]
        payload: Option<PathBuf>,
    },
    /// Send the image to the engraver
    Upload(UploadArgs),
    /// Send the image and start engraving. SWITCHES THE LASER ON
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
    /// Restart the engraver (Reboot). With --port it is sent even if the engraver does not answer
    Reset,
    /// Go to the origin (V10)
    Home,
    /// Go to the centre
    Center,
    /// Trace the outline of the image. SWITCHES THE LASER ON
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
    /// Move the head
    Move {
        #[arg(value_enum)]
        direction: DirectionArg,
        #[arg(default_value_t = 1)]
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
    Monitor,
    /// Run a virtual engraver for tests (Unix)
    Simulate {
        #[arg(long, value_enum, default_value = "v10")]
        sim_protocol: SimProtocol,
        /// Where to save the image the virtual engraver receives
        #[arg(short, long, default_value = "virtual-engraving.png")]
        output: PathBuf,
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

    fn upload(&self, engraver: &Engraver, args: &UploadArgs) -> Result<()> {
        let image = args.image.job().build(engraver.protocol().base_size())?;
        if let Some(path) = &args.save_preview {
            image.save(path).with_context(|| format!("cannot save {}", path.display()))?;
        }
        println!("{}", tr("Erasing the memory...", "Cancellazione della memoria..."));
        let wait = args.erase_wait.map(Duration::try_from_secs_f64).transpose().context("invalid --erase-wait")?;
        engraver.upload(&image, wait, |sent, total| {
            print!("\r  {:3}%  ({sent}/{total} byte)", sent * 100 / total);
            // Progress output only: a failed flush is not worth stopping the upload.
            let _ = io::stdout().flush();
        })?;
        println!("\n{}", tr("Image sent", "Immagine inviata"));
        Ok(())
    }

    fn run(&self, engraver: &Engraver, args: &RunArgs) -> Result<()> {
        println!(
            "{} {}",
            tr("Starting the engraving, burn time", "Avvio dell'incisione, tempo di bruciatura"),
            args.burn
        );
        engraver.start(args.burn)?;
        let started = Instant::now();
        let mut last_shown = Instant::now() - Duration::from_secs(2);
        let timeout = args.timeout.map(Duration::try_from_secs_f64).transpose().context("invalid --timeout")?;
        let end = engraver.wait_for(
            |event| matches!(event, Event::Finished | Event::Disconnected),
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
        } else if end.is_none() {
            println!(
                "\n{}",
                tr("No end message within the time limit", "Nessun messaggio di fine entro il tempo massimo")
            );
        }
        Ok(())
    }
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
            "This command SWITCHES THE LASER ON AT FULL POWER. Before continuing:\n  - wear the protective goggles;\n  - put the material under the head;\n  - keep anything flammable away.",
            "Questo comando ACCENDE IL LASER A PIENA POTENZA. Prima di continuare:\n  - indossa gli occhiali protettivi;\n  - metti il materiale sotto la testa;\n  - allontana tutto ciò che è infiammabile.",
        )
    );
    print!("{} ", tr("Type \"yes\" to continue:", "Scrivi \"si\" per continuare:"));
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    match answer.trim().to_lowercase().as_str() {
        "yes" | "y" | "si" | "sì" | "s" => Ok(()),
        _ => bail!(tr("cancelled", "annullato")),
    }
}

fn describe(event: &Event) -> String {
    match *event {
        Event::Connected { hw: Some(hw) } => {
            format!("{} (hardware {hw})", tr("Connected", "Connesso"))
        }
        Event::Connected { hw: None } => tr("Connected", "Connesso").to_owned(),
        Event::Progress { x, y } => format!("X:{x} Y:{y}"),
        Event::Finished => tr("Engraving finished", "Incisione terminata").to_owned(),
        Event::NoPower => {
            tr("No power: plug in the 5 V power supply", "Alimentazione assente: collega l'alimentatore 5 V").to_owned()
        }
        Event::PowerOk => tr("Power supply present", "Alimentazione presente").to_owned(),
        Event::EraseDone => tr("Memory erased", "Memoria cancellata").to_owned(),
        Event::TransferImage => tr("Ready to receive the image", "Pronto a ricevere l'immagine").to_owned(),
        Event::Version(v) => format!("{}: {v}", tr("Hardware version", "Versione hardware")),
        Event::LowBatterySleep => tr("Sleeping: battery low", "In sospensione: batteria scarica").to_owned(),
        Event::TimeoutSleep => tr("Sleeping: idle for too long", "In sospensione: inattivo da troppo tempo").to_owned(),
        Event::Debug(v) => format!("Debug: {v}"),
        Event::Battery(v) => format!("{}: {v}", tr("Battery", "Batteria")),
        Event::BurnValue(v) => format!("{}: {v}", tr("Burn time on the machine", "Tempo di bruciatura sulla macchina")),
        Event::VerifyOk => tr("Verification OK", "Verifica OK").to_owned(),
        Event::Unknown(b) => format!("{}: {b:#04x}", tr("Unknown byte", "Byte sconosciuto")),
        Event::Disconnected => tr("Connection lost", "Connessione persa").to_owned(),
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

fn preview(app: &App, image: &ImageArgs, output: &PathBuf, payload: Option<&PathBuf>) -> Result<()> {
    let protocol = app.offline_protocol();
    let img = image.job().build(protocol.base_size())?;
    img.save(output).with_context(|| format!("cannot save {}", output.display()))?;
    println!(
        "{} {} ({}x{}, {:.1}% {}, {} {:.2} mm)",
        tr("Preview saved in", "Anteprima salvata in"),
        output.display(),
        img.width(),
        img.height(),
        imaging::black_ratio(&img) * 100.0,
        tr("of the pixels to burn", "di pixel da bruciare"),
        tr("side", "lato"),
        f64::from(img.width()) * neje::gcode::MM_PER_PX,
    );
    if let Some(path) = payload {
        std::fs::write(path, protocol.build_payload(&img))
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

fn monitor(app: &App) -> Result<()> {
    let engraver = app.connect()?;
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
    let options = Options { version, hw: app.hw.unwrap_or(11), out_png: Some(output), ..Options::default() };
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
        Cmd::Preview { image, output, payload } => preview(&app, &image, &output, payload.as_ref()),
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
            app.upload(&engraver, &upload)?;
            thread::sleep(device::AFTER_UPLOAD_PAUSE);
            app.run(&engraver, &run)
        }
        Cmd::Start(args) => {
            confirm_laser(args.laser.yes)?;
            app.run(&app.connect()?, &args)
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
        Cmd::Monitor => monitor(&app),
        Cmd::Simulate { sim_protocol, output } => simulate(&app, sim_protocol, output),
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
