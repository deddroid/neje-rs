//! The window: preview on the left, controls on the right. Everything that touches the
//! serial port goes through the [`Worker`] thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use ab_glyph::FontVec;
use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke};
use image::{GrayImage, RgbImage};
use neje::device::{self, PortInfo};
use neje::doctor::{self, Problem};
use neje::fonts::{self, FontEntry};
use neje::gcode;
use neje::i18n::{self, Lang, tr};
use neje::imaging::{self, Mode};
use neje::messages::{describe, laser_checklist, mode_description, problem_fix_label, problem_text, problem_title};
use neje::plan::{self, Plan, PrintMode};
use neje::protocol::{BURN_MAX, BURN_MIN, Command, Direction, Event, MEASURED_MM_PER_PX, Protocol, Version};
use neje::scale::Baseline;
use neje::text::{self, FontSource};

use crate::settings::Settings;
use crate::theme::{self, palette};
use crate::worker::{Request, Update, Worker};

const PANEL_WIDTH: f32 = 368.0;
const LOG_LINES: usize = 200;
/// Height of the strip with the three print modes under the preview.
const MODE_STRIP_HEIGHT: f32 = 156.0;
/// Positions kept for the red trace on the preview.
const MAX_DOTS: usize = 40_000;
/// Smallest em size of a text, in pixels.
const MIN_TEXT: u32 = 6;
/// Side of the corner handles of the floating text, in points.
const HANDLE: f32 = 10.0;
const IMAGE_EXTENSIONS: [&str; 8] = ["jpg", "jpeg", "png", "bmp", "gif", "tif", "tiff", "webp"];

/// What the window was asked to do at start-up.
#[derive(Default)]
pub struct Startup {
    pub port: Option<String>,
    pub protocol: Option<ProtocolChoice>,
    pub image: Option<PathBuf>,
    pub print_mode: Option<ModeKind>,
    pub stroke: Option<u8>,
    /// Folder for the automatic screenshots (one per theme and language), then quit.
    pub screenshots: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum ProtocolChoice {
    V10,
    V20,
    V21,
    V30,
    /// Try every protocol in the order of the original launcher.
    Auto,
}

impl ProtocolChoice {
    const ALL: [ProtocolChoice; 5] =
        [ProtocolChoice::V10, ProtocolChoice::V20, ProtocolChoice::V21, ProtocolChoice::V30, ProtocolChoice::Auto];

    fn versions(self) -> Vec<Version> {
        match self {
            ProtocolChoice::V10 => vec![Version::V10],
            ProtocolChoice::V20 => vec![Version::V20],
            ProtocolChoice::V21 => vec![Version::V21],
            ProtocolChoice::V30 => vec![Version::V30],
            ProtocolChoice::Auto => Version::DETECTION_ORDER.to_vec(),
        }
    }

    fn label(self) -> String {
        match self {
            ProtocolChoice::V10 => "V10 (DK-8-KZ)".to_owned(),
            ProtocolChoice::V20 => "V20".to_owned(),
            ProtocolChoice::V21 => "V21".to_owned(),
            ProtocolChoice::V30 => "V30".to_owned(),
            ProtocolChoice::Auto => tr("Try them all", "Provali tutti").to_owned(),
        }
    }
}

/// The print modes offered, without their parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum ModeKind {
    Single,
    Split,
    Line,
}

impl ModeKind {
    const ALL: [ModeKind; 3] = [ModeKind::Single, ModeKind::Split, ModeKind::Line];

    fn mode(self, stroke: u8) -> PrintMode {
        match self {
            ModeKind::Single => PrintMode::Single,
            ModeKind::Split => PrintMode::Split,
            ModeKind::Line => PrintMode::Line { stroke },
        }
    }

    fn label(self) -> &'static str {
        match self {
            ModeKind::Single => tr("Single job", "Lavoro unico"),
            ModeKind::Split => tr("Split", "Diviso"),
            ModeKind::Line => tr("Line art", "A tratto"),
        }
    }
}

#[derive(Clone, PartialEq)]
enum Link {
    Disconnected,
    Connecting,
    Connected { port: String, protocol: Protocol },
}

/// A command waiting for the laser safety confirmation.
#[derive(Clone, Copy, PartialEq)]
enum LaserAction {
    Start,
    /// Send and engrave every job of the current plan.
    Jobs,
    Command(Command),
}

/// Text being placed: drawn over the preview, outside `base`, until it is placed.
struct FloatingText {
    text: String,
    font: FontSource,
    /// `font`, loaded.
    face: FontVec,
    /// Em size in pixels.
    size: u32,
    /// Centre on the work area, in pixels.
    centre: egui::Pos2,
    stamp: GrayImage,
    texture: Option<egui::TextureHandle>,
    /// Set when the stamp must be drawn again.
    dirty: bool,
    /// Height asked in the window, in mm; kept while the text or the font change.
    asked_mm: Option<f64>,
    /// Em size and pointer distance from the centre when a corner drag started.
    resize_from: Option<(u32, f32)>,
    /// Filter of the font list.
    filter: String,
}

impl FloatingText {
    fn new(font: FontSource, face: FontVec, centre: f32) -> Self {
        FloatingText {
            text: String::new(),
            font,
            face,
            size: 48,
            centre: egui::pos2(centre, centre),
            stamp: GrayImage::from_pixel(1, 1, image::Luma([255])),
            texture: None,
            dirty: true,
            asked_mm: None,
            resize_from: None,
            filter: String::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    fn set_size(&mut self, size: u32) {
        let size = size.clamp(MIN_TEXT, text::MAX_SIZE);
        if size != self.size {
            self.size = size;
            self.dirty = true;
        }
    }

    /// Applies the height asked in mm, if any, to the current text and font.
    fn fit_height(&mut self) {
        if let Some(mm) = self.asked_mm
            && !self.is_empty()
        {
            let px = (mm / MEASURED_MM_PER_PX).round().max(1.0) as u32;
            self.set_size(text::size_for_height(&self.face, &self.text, px));
        }
    }

    /// Draws the stamp again if something changed, and its texture for the preview.
    fn refresh(&mut self, ctx: &egui::Context) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.stamp = text::render_with(&self.face, &self.text, self.size);
        // The GPU refuses textures larger than `max_texture_side`: a long line at a large height
        // is shown from a smaller copy, stretched back over the stamp on the preview.
        let max = ctx.input(|i| i.max_texture_side) as f64;
        let (w, h) = (f64::from(self.stamp.width()), f64::from(self.stamp.height()));
        let shrunk = (w > max || h > max).then(|| {
            let ratio = (max / w).min(max / h);
            let (sw, sh) = (((w * ratio) as u32).max(1), ((h * ratio) as u32).max(1));
            image::imageops::resize(&self.stamp, sw, sh, image::imageops::FilterType::Triangle)
        });
        let shown = shrunk.as_ref().unwrap_or(&self.stamp);
        // Ink opaque, paper transparent: the preview tints it with the accent colour.
        let rgba: Vec<u8> = shown.pixels().flat_map(|p| if p.0[0] < 128 { [255; 4] } else { [0; 4] }).collect();
        let image = egui::ColorImage::from_rgba_unmultiplied([shown.width() as usize, shown.height() as usize], &rgba);
        match &mut self.texture {
            Some(texture) => texture.set(image, egui::TextureOptions::NEAREST),
            None => self.texture = Some(ctx.load_texture("floating-text", image, egui::TextureOptions::NEAREST)),
        }
    }

    /// Centre in whole pixels, as given to `imaging::place`.
    fn anchor(&self) -> (i64, i64) {
        (self.centre.x.round() as i64, self.centre.y.round() as i64)
    }

    /// Top left corner on the work area, where `imaging::place` puts the stamp.
    fn top_left(&self) -> egui::Vec2 {
        let (cx, cy) = self.anchor();
        egui::vec2(
            (cx - i64::from(self.stamp.width()).div_euclid(2)) as f32,
            (cy - i64::from(self.stamp.height()).div_euclid(2)) as f32,
        )
    }
}

/// The installed fonts, found by a thread of their own at start-up.
enum Fonts {
    Scanning(Receiver<Vec<FontEntry>>),
    Ready(Vec<FontEntry>),
}

/// Name of `font` as shown in the text window.
fn font_label(font: &FontSource, fonts: &[FontEntry]) -> String {
    match font {
        FontSource::BuiltIn => tr("Inter (built-in)", "Inter (integrato)").to_owned(),
        FontSource::File { path, index } => fonts
            .iter()
            .find(|f| &f.path == path && f.index == *index)
            .map_or_else(|| file_name(path), FontEntry::label),
    }
}

enum Picked {
    Image(PathBuf),
    Gcode(PathBuf),
    SavePreview(PathBuf),
    /// Outcome of the fix of a system problem.
    Fixed(Result<(), String>),
}

/// What a screenshot shows besides the window.
#[derive(Clone, Copy, PartialEq)]
enum Extra {
    Nothing,
    LaserDialog,
    /// The banner about a missing permission.
    Banner,
}

struct Screenshots {
    dir: PathBuf,
    steps: Vec<(egui::Theme, Lang, Extra)>,
    index: usize,
    frames: u32,
    requested: bool,
    tx: Sender<Arc<egui::ColorImage>>,
    rx: Receiver<Arc<egui::ColorImage>>,
}

pub struct App {
    worker: Worker,
    settings: Settings,

    /// The composition: what the user opened, rotated, wrote. Black = burn.
    base: GrayImage,
    /// The current plan, as drawn in the preview.
    texture: egui::TextureHandle,
    print_mode: ModeKind,
    stroke: u8,
    /// `base` prepared for each print mode, in the order of [`ModeKind::ALL`].
    plans: [Plan; 3],
    thumbnails: [egui::TextureHandle; 3],
    /// Pictures of `plans`, kept to redraw the preview when the selection changes.
    pictures: [egui::ColorImage; 3],
    /// Set when `base`, the burn time, the stroke or the protocol change.
    plans_dirty: bool,
    /// The plans are computed by a thread of their own: the thinning and the search of the
    /// bands take up to a tenth of a second, too long for the window.
    planner: Sender<PlanRequest>,
    planned: Receiver<PlanResult>,
    /// Number of the last request sent to the planner and of the last result applied.
    requested: u64,
    applied: u64,
    /// The picture as opened, to convert it again when the mode changes.
    original: Option<RgbImage>,
    mode: Mode,
    gcode_fit: bool,
    /// Bounding box of the ink of `base`.
    ink: Option<(u32, u32, u32, u32)>,
    /// True while `base` holds only the opened picture, as converted.
    picture_only: bool,
    /// The 100 % state the size fields scale from; `None` until the size is first changed.
    baseline: Option<Baseline>,
    /// Size asked in the fields (width, height in mm) while scaling from `baseline`.
    size_mm: Option<(f64, f64)>,
    /// Text being placed over the preview.
    floating: Option<FloatingText>,
    fonts: Fonts,

    link: Link,
    /// What the system lacks for the engraver (driver, permission), shown as a banner.
    problems: Vec<Problem>,
    /// True while the fix of a problem is running.
    fixing: bool,
    ports: Vec<PortInfo>,
    port_choice: Option<String>,
    protocol_choice: ProtocolChoice,

    burn: u8,
    /// Fraction sent, while an image is going to the engraver.
    upload: Option<f32>,
    /// The image being sent, and the last one the engraver received.
    uploading: Option<GrayImage>,
    sent: Option<GrayImage>,
    /// True when the engraver holds the image of the current plan (a plan of one job).
    image_sent: bool,
    /// Job in progress and number of jobs, while a sequence runs.
    sequence: Option<(usize, usize)>,
    engraving_since: Option<Instant>,
    last_duration: Option<Duration>,
    position: Option<(i32, i32)>,
    dots: Vec<(i32, i32)>,
    confirm: Option<LaserAction>,

    log: Vec<String>,
    picked_tx: Sender<Picked>,
    picked_rx: Receiver<Picked>,
    shots: Option<Screenshots>,
    /// True when a widget had the keyboard focus at the end of the last frame.
    had_focus: bool,
}

struct PlanRequest {
    number: u64,
    base: GrayImage,
    stroke: u8,
    burn: u8,
    protocol: Protocol,
}

struct PlanResult {
    number: u64,
    plans: [Plan; 3],
    pictures: [egui::ColorImage; 3],
}

/// Computes the plans asked through `requests`, skipping those already superseded.
fn planner(requests: &Receiver<PlanRequest>, results: &Sender<PlanResult>, ctx: &egui::Context) {
    while let Ok(mut request) = requests.recv() {
        while let Ok(newer) = requests.try_recv() {
            request = newer;
        }
        let PlanRequest { number, base, stroke, burn, protocol } = request;
        let plans = ModeKind::ALL.map(|kind| Plan::new(&base, kind.mode(stroke), burn, protocol));
        let pictures = [plan_picture(&plans[0]), plan_picture(&plans[1]), plan_picture(&plans[2])];
        if results.send(PlanResult { number, plans, pictures }).is_err() {
            return; // the window is gone
        }
        ctx.request_repaint();
    }
}

fn plan_picture(plan: &Plan) -> egui::ColorImage {
    let picture = plan.preview();
    egui::ColorImage::from_rgb([picture.width() as usize, picture.height() as usize], picture.as_raw())
}

fn colour(rgb: image::Rgb<u8>) -> Color32 {
    Color32::from_rgb(rgb.0[0], rgb.0[1], rgb.0[2])
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// A length in millimetres, up to `max`.
fn mm_field(value: &mut f64, max: f64) -> egui::DragValue<'_> {
    // Typed values apply on Enter or focus loss: applying each keystroke would clamp partial input
    // and re-render the whole area while typing.
    // Without `clamp_existing_to_range(false)` a value shown out of range is clamped and reported
    // as changed with no input at all.
    egui::DragValue::new(value)
        .speed(0.05)
        .range(0.1..=max)
        .fixed_decimals(1)
        .suffix(" mm")
        .update_while_editing(false)
        .clamp_existing_to_range(false)
}

fn px_to_mm(px: u32) -> f64 {
    f64::from(px) * MEASURED_MM_PER_PX
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        i18n::set(settings.lang());
        let protocol = Protocol::new(Version::V10, None);
        let base = imaging::new_base(protocol.base_size());
        let burn = settings.burn.clamp(BURN_MIN, BURN_MAX);
        let stroke = startup.stroke.unwrap_or(1).clamp(1, plan::MAX_STROKE);
        let plans = ModeKind::ALL.map(|kind| Plan::new(&base, kind.mode(stroke), burn, protocol));
        let pictures = [plan_picture(&plans[0]), plan_picture(&plans[1]), plan_picture(&plans[2])];
        let load = |name: &str, picture: &egui::ColorImage| {
            cc.egui_ctx.load_texture(name, picture.clone(), egui::TextureOptions::LINEAR)
        };
        let texture = load("preview", &pictures[0]);
        let thumbnails = [load("single", &pictures[0]), load("split", &pictures[1]), load("line", &pictures[2])];
        let (planner_tx, planner_rx) = mpsc::channel();
        let (planned_tx, planned) = mpsc::channel();
        let planner_ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || planner(&planner_rx, &planned_tx, &planner_ctx));
        let (picked_tx, picked_rx) = mpsc::channel();
        let shots = startup.screenshots.map(|dir| {
            let (tx, rx) = mpsc::channel();
            let mut steps: Vec<_> = [egui::Theme::Dark, egui::Theme::Light]
                .into_iter()
                .flat_map(|theme| Lang::ALL.map(|lang| (theme, lang, Extra::Nothing)))
                .collect();
            steps.extend([
                (egui::Theme::Dark, Lang::En, Extra::LaserDialog),
                (egui::Theme::Light, Lang::It, Extra::LaserDialog),
                (egui::Theme::Dark, Lang::En, Extra::Banner),
                (egui::Theme::Light, Lang::It, Extra::Banner),
            ]);
            Screenshots { dir, steps, index: 0, frames: 0, requested: false, tx, rx }
        });
        let (fonts_tx, fonts_rx) = mpsc::channel();
        let fonts_ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            // The window may have closed meanwhile; then the list no longer matters.
            let _ = fonts_tx.send(fonts::scan());
            fonts_ctx.request_repaint();
        });
        let mut app = App {
            worker: Worker::spawn(cc.egui_ctx.clone()),
            burn,
            settings,
            base,
            texture,
            print_mode: startup.print_mode.unwrap_or(ModeKind::Single),
            stroke,
            plans,
            thumbnails,
            pictures,
            plans_dirty: true,
            planner: planner_tx,
            planned,
            requested: 0,
            applied: 0,
            original: None,
            mode: Mode::Bw,
            gcode_fit: true,
            ink: None,
            picture_only: false,
            baseline: None,
            size_mm: None,
            floating: None,
            fonts: Fonts::Scanning(fonts_rx),
            link: Link::Disconnected,
            problems: Vec::new(),
            fixing: false,
            ports: Vec::new(),
            port_choice: startup.port.clone(),
            protocol_choice: startup.protocol.unwrap_or(ProtocolChoice::V10),
            upload: None,
            uploading: None,
            sent: None,
            image_sent: false,
            sequence: None,
            engraving_since: None,
            last_duration: None,
            position: None,
            dots: Vec::new(),
            confirm: None,
            log: Vec::new(),
            picked_tx,
            picked_rx,
            shots,
            had_focus: false,
        };
        app.refresh_ports();
        if let Some(path) = startup.image {
            app.open_path(&path);
        }
        if startup.port.is_some() {
            app.connect();
        }
        app
    }

    // ---- state -----------------------------------------------------------------------

    fn log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > LOG_LINES {
            self.log.drain(..self.log.len() - LOG_LINES);
        }
    }

    fn log_error(&mut self, error: impl std::fmt::Display) {
        self.log(format!("{}: {error}", tr("Error", "Errore")));
    }

    fn protocol(&self) -> Option<Protocol> {
        match &self.link {
            Link::Connected { protocol, .. } => Some(*protocol),
            _ => None,
        }
    }

    fn base_size(&self) -> u32 {
        self.base.width()
    }

    /// A new composition: what was on the work area before is the new 100 %.
    fn set_base(&mut self, base: GrayImage) {
        self.show_base(base);
        self.baseline = None;
        self.size_mm = None;
        self.picture_only = false;
    }

    /// Shows `base` keeping the starting point of the scaling.
    fn show_base(&mut self, base: GrayImage) {
        self.ink = imaging::ink_bounds(&base);
        self.base = base;
        self.plans_dirty = true;
        self.dots.clear();
        self.position = None;
    }

    /// The plan of the selected print mode.
    fn plan(&self) -> &Plan {
        &self.plans[self.selected()]
    }

    fn selected(&self) -> usize {
        ModeKind::ALL.iter().position(|&k| k == self.print_mode).unwrap_or(0)
    }

    /// False while the plans shown are older than the image, the burn time or the stroke.
    fn plans_ready(&self) -> bool {
        !self.plans_dirty && self.applied == self.requested
    }

    /// Asks the planner for new plans when something changed, and applies its answers.
    fn refresh_plans(&mut self) {
        if self.plans_dirty {
            self.plans_dirty = false;
            self.requested += 1;
            let request = PlanRequest {
                number: self.requested,
                base: self.base.clone(),
                stroke: self.stroke,
                burn: self.burn,
                protocol: self.protocol().unwrap_or(Protocol::new(Version::V10, None)),
            };
            // The planner lives as long as the window.
            let _ = self.planner.send(request);
        }
        while let Ok(result) = self.planned.try_recv() {
            if result.number != self.requested {
                continue; // superseded while it was being computed
            }
            self.applied = result.number;
            self.plans = result.plans;
            self.pictures = result.pictures;
            for (thumbnail, picture) in self.thumbnails.iter_mut().zip(&self.pictures) {
                thumbnail.set(picture.clone(), egui::TextureOptions::LINEAR);
            }
            self.show_selected_plan();
        }
    }

    /// Draws the plan of the selected print mode in the preview.
    fn show_selected_plan(&mut self) {
        self.texture.set(self.pictures[self.selected()].clone(), egui::TextureOptions::LINEAR);
        self.update_image_sent();
    }

    fn update_image_sent(&mut self) {
        let plan = self.plan();
        self.image_sent = plan.jobs.len() == 1 && self.sent.as_ref() == Some(&plan.jobs[0].image);
    }

    /// The work area of the connected engraver differs from the preview: start again.
    fn resize_base(&mut self, base_size: u32) {
        if base_size != self.base_size() {
            self.set_base(imaging::new_base(base_size));
            self.reapply();
        }
    }

    /// `button_Reload`: the opened picture again, converted with the current mode.
    fn reapply(&mut self) {
        let size = self.base_size();
        let mut base = imaging::new_base(size);
        if let Some(original) = &self.original {
            base = imaging::center_on(&base, &imaging::prepare(original, size, self.mode));
        }
        self.set_base(base);
        self.picture_only = self.original.is_some();
    }

    /// Scales the content so that its width (`width`) or height is `mm`, from the 100 % state.
    fn scale_to(&mut self, mm: f64, width: bool) {
        if self.baseline.is_none() {
            self.baseline = match (&self.original, self.picture_only) {
                (Some(original), true) => Baseline::picture(original, self.mode, self.base_size()),
                _ => Baseline::bitmap(&self.base),
            };
        }
        let Some(baseline) = &self.baseline else { return };
        let (bw, bh) = baseline.ink_size();
        let along = if width { bw } else { bh };
        let requested = mm / px_to_mm(along);
        let factor = baseline.clamp_factor(requested);
        let (w, h) = baseline.size(factor);
        let image = baseline.render(w, h);
        self.show_base(image);
        // The fields keep what was asked rather than the whole pixels it became: snapping them
        // would fight the dragging of the values. The edited one stays exactly `mm`, as the field
        // drops what is being typed when the value it gets back differs from it.
        let (bw, bh) = (f64::from(bw), f64::from(bh));
        self.size_mm = Some(if factor != requested {
            (bw * factor * MEASURED_MM_PER_PX, bh * factor * MEASURED_MM_PER_PX)
        } else if width {
            (mm, mm * bh / bw)
        } else {
            (mm * bw / bh, mm)
        });
    }

    fn poll_fonts(&mut self) {
        if let Fonts::Scanning(rx) = &self.fonts {
            match rx.try_recv() {
                Ok(list) => self.fonts = Fonts::Ready(list),
                // The scan thread died: carry on with the built-in font only.
                Err(TryRecvError::Disconnected) => self.fonts = Fonts::Ready(Vec::new()),
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    /// Starts a floating text in the middle of the work area, in the last font used.
    fn open_text(&mut self) {
        let remembered = self.settings.font_source();
        let (font, face) = match text::load(&remembered) {
            Ok(face) => (remembered, face),
            Err(e) => {
                self.log_error(e);
                (FontSource::BuiltIn, text::load(&FontSource::BuiltIn).expect("the built-in font loads"))
            }
        };
        self.floating = Some(FloatingText::new(font, face, self.base_size() as f32 / 2.0));
    }

    /// Burns the floating text into the composition.
    fn place_text(&mut self) {
        let Some(float) = self.floating.take() else { return };
        if float.is_empty() {
            return;
        }
        let stamp =
            if float.dirty { text::render_with(&float.face, &float.text, float.size) } else { float.stamp.clone() };
        let (cx, cy) = float.anchor();
        let base = imaging::place(&self.base, &stamp, cx, cy);
        self.set_base(base);
        self.settings.remember_font(&float.font);
        self.save_settings();
    }

    /// Lists the ports again and checks driver and permissions (reads `/sys`, opens nothing).
    fn refresh_ports(&mut self) {
        match device::ports() {
            Ok(ports) => self.ports = ports,
            Err(e) => self.log_error(e),
        }
        if self.shots.is_none() {
            self.problems = doctor::check();
        }
    }

    /// Runs the fix of `problem` as administrator, in its own thread: the password window
    /// can stay open for a long time.
    fn fix(&mut self, ctx: &egui::Context, problem: &Problem) {
        let Some(fix) = problem.fix() else { return };
        let Some(elevator) = doctor::elevator(true) else {
            self.log_error(tr(
                "neither pkexec nor sudo found: run \"neje doctor --fix\" as administrator",
                "né pkexec né sudo trovati: esegui \"neje doctor --fix\" come amministratore",
            ));
            return;
        };
        self.fixing = true;
        let (tx, ctx) = (self.picked_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            // The window may have closed meanwhile; then the outcome no longer matters.
            let _ = tx.send(Picked::Fixed(fix.run(elevator)));
            ctx.request_repaint();
        });
    }

    fn connect(&mut self) {
        self.link = Link::Connecting;
        self.log(tr("Looking for the engraver...", "Ricerca dell'incisore..."));
        self.worker
            .request(Request::Connect { port: self.port_choice.clone(), versions: self.protocol_choice.versions() });
    }

    fn send(&mut self, command: Command) {
        if command.fires_laser() {
            self.confirm = Some(LaserAction::Command(command));
        } else {
            self.worker.request(Request::Send(command));
        }
    }

    fn open_path(&mut self, path: &Path) {
        if gcode::is_gcode_path(path) {
            self.open_gcode(path);
        } else {
            self.open_image(path);
        }
    }

    fn open_image(&mut self, path: &Path) {
        match imaging::load(path) {
            Ok(image) => {
                self.original = Some(image);
                self.reapply();
                self.log(format!("{}: {}", tr("Picture", "Immagine"), file_name(path)));
            }
            Err(e) => self.log_error(e),
        }
    }

    fn open_gcode(&mut self, path: &Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                self.original = None;
                self.set_base(gcode::rasterize(&String::from_utf8_lossy(&bytes), self.base_size(), self.gcode_fit, 1));
                self.log(format!("G-code: {}", file_name(path)));
            }
            Err(e) => self.log_error(format!("{}: {e}", path.display())),
        }
    }

    /// Opens a file dialog in its own thread; the answer arrives through `picked_rx`.
    fn pick(
        &self,
        ctx: &egui::Context,
        what: fn(PathBuf) -> Picked,
        dialog: impl FnOnce() -> Option<PathBuf> + Send + 'static,
    ) {
        let (tx, ctx) = (self.picked_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            if let Some(path) = dialog() {
                // The window may have closed meanwhile; then the choice no longer matters.
                let _ = tx.send(what(path));
                ctx.request_repaint();
            }
        });
    }

    // ---- polling ---------------------------------------------------------------------

    fn poll_worker(&mut self) {
        while let Ok(update) = self.worker.updates.try_recv() {
            match update {
                Update::Connected { port, protocol } => {
                    let hw = protocol.hw.map(|hw| format!(", hardware {hw}")).unwrap_or_default();
                    self.log(format!(
                        "{} {port}: {} {}{hw}",
                        tr("Connected to", "Connesso a"),
                        tr("protocol", "protocollo"),
                        protocol.version
                    ));
                    self.link = Link::Connected { port, protocol };
                    self.plans_dirty = true;
                    self.resize_base(protocol.base_size());
                }
                Update::Disconnected => {
                    if self.link != Link::Disconnected {
                        self.log(tr("Disconnected", "Disconnesso"));
                    }
                    self.link = Link::Disconnected;
                    self.upload = None;
                    self.sent = None;
                    self.sequence = None;
                    self.plans_dirty = true;
                    self.stop_clock();
                }
                Update::Event(event) => self.on_event(event),
                Update::Sent(command) => self.log(format!("{}: {}", tr("Sent", "Inviato"), command.name())),
                Update::Started => {
                    self.engraving_since = Some(Instant::now());
                    self.last_duration = None;
                    // The trace of the previous jobs of a sequence stays on the preview.
                    if self.sequence.is_none_or(|(index, _)| index == 1) {
                        self.dots.clear();
                    }
                    self.log(format!(
                        "{} {}",
                        tr("Engraving started, burn time", "Incisione avviata, tempo di bruciatura"),
                        self.burn
                    ));
                }
                Update::UploadProgress { sent, total } => self.upload = Some(sent as f32 / total.max(1) as f32),
                Update::UploadDone => {
                    self.upload = None;
                    self.sent = self.uploading.take();
                    self.update_image_sent();
                    self.log(tr("Image sent to the engraver", "Immagine inviata all'incisore"));
                }
                Update::Job { index, total } => {
                    self.sequence = Some((index, total));
                    self.upload = Some(0.0);
                    self.sent = None;
                    self.update_image_sent();
                    self.log(format!(
                        "{} {index}/{total}: {}",
                        tr("Job", "Lavoro"),
                        tr("erasing the memory...", "cancellazione della memoria...")
                    ));
                }
                Update::JobsDone => {
                    self.sequence = None;
                    self.log(tr("All the jobs are finished", "Tutti i lavori sono terminati"));
                }
                Update::Restarted => {
                    self.sequence = None;
                    self.upload = None;
                    self.stop_clock();
                    self.log_error(tr(
                        "the engraver restarted during the job, which is incomplete. Power it from a 5 V supply, not from the USB ports of the computer",
                        "l'incisore si è riavviato durante il lavoro, che è incompleto. Alimentalo con un alimentatore 5 V, non dalle porte USB del computer",
                    ));
                }
                Update::Error(error) => {
                    self.upload = None;
                    self.uploading = None;
                    self.sequence = None;
                    if self.link == Link::Connecting {
                        self.link = Link::Disconnected;
                        self.refresh_ports(); // a failed search may be a missing driver or permission
                    }
                    self.log_error(error);
                }
            }
        }
    }

    fn stop_clock(&mut self) {
        if let Some(since) = self.engraving_since.take() {
            self.last_duration = Some(since.elapsed());
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Progress { x, y } => {
                self.position = Some((x, y));
                if self.dots.len() >= MAX_DOTS {
                    self.dots.drain(..MAX_DOTS / 2);
                }
                self.dots.push((x, y));
            }
            // Repeated at every handshake; the original ignores unknown bytes too.
            Event::Connected { hw: None } | Event::Unknown(_) | Event::Disconnected => {}
            Event::Connected { hw: Some(hw) } => {
                if let Link::Connected { protocol, .. } = &mut self.link {
                    protocol.hw = Some(hw);
                    let size = protocol.base_size();
                    self.resize_base(size);
                }
            }
            Event::Finished => {
                self.stop_clock();
                self.log(describe(&event));
            }
            other => self.log(describe(&other)),
        }
    }

    fn poll_files(&mut self, ctx: &egui::Context) {
        while let Ok(picked) = self.picked_rx.try_recv() {
            match picked {
                Picked::Image(path) => self.open_image(&path),
                Picked::Gcode(path) => self.open_gcode(&path),
                Picked::Fixed(outcome) => {
                    self.fixing = false;
                    match outcome {
                        Ok(()) => self.log(tr(
                            "Done. If the engraver is still not found, unplug it and plug it in again.",
                            "Fatto. Se l'incisore non viene ancora trovato, scollegalo e ricollegalo.",
                        )),
                        Err(e) => self.log_error(e),
                    }
                    self.refresh_ports();
                }
                Picked::SavePreview(path) => match self.plan().image.save(&path) {
                    Ok(()) => {
                        self.log(format!("{} {}", tr("Preview saved in", "Anteprima salvata in"), path.display()))
                    }
                    Err(e) => self.log_error(format!("{}: {e}", path.display())),
                },
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if let Some(path) = dropped.first() {
            self.open_path(path);
        }
    }

    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let connecting = self.link == Link::Connecting || !self.plans_ready();
        let laser = if self.plan().jobs.len() > 1 { LaserAction::Jobs } else { LaserAction::Start };
        let Some(s) = self.shots.as_mut() else { return };
        ctx.request_repaint();
        if connecting {
            return; // wait for the engraver, so the picture shows the connected state
        }
        if let Ok(image) = s.rx.try_recv() {
            let (theme, lang, extra) = s.steps[s.index];
            let theme = if theme == egui::Theme::Dark { "dark" } else { "light" };
            let suffix = match extra {
                Extra::Nothing => "",
                Extra::LaserDialog => "_laser",
                Extra::Banner => "_banner",
            };
            let path = s.dir.join(format!("{}_{theme}{suffix}.png", lang.code()));
            let [w, h] = image.size;
            if let Err(e) = image::save_buffer(&path, image.as_raw(), w as u32, h as u32, image::ColorType::Rgba8) {
                eprintln!("{}: {e}", path.display());
            }
            s.index += 1;
            s.frames = 0;
            s.requested = false;
        }
        let Some(&(theme, lang, extra)) = s.steps.get(s.index) else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        ctx.set_theme(theme);
        i18n::set(lang);
        self.confirm = (extra == Extra::LaserDialog).then_some(laser);
        self.problems = if extra == Extra::Banner {
            vec![Problem::NoAccess { port: "/dev/ttyUSB0".to_owned() }]
        } else {
            Vec::new()
        };
        s.frames += 1;
        if s.frames >= 5 && !s.requested {
            s.requested = true;
            let (tx, repaint) = (s.tx.clone(), ctx.clone());
            ctx.request_screenshot(move |image| {
                // The receiver lives as long as the window.
                let _ = tx.send(image);
                repaint.request_repaint();
            });
        }
    }

    // ---- preview ---------------------------------------------------------------------

    fn preview(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new("NEJE DK-8-KZ").font(theme::title_font(26.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (color, text) = match &self.link {
                    Link::Disconnected => (p.text_dim, tr("Not connected", "Non connesso").to_owned()),
                    Link::Connecting => (p.warn, tr("Searching...", "Ricerca...").to_owned()),
                    Link::Connected { port, protocol } => (p.ok, format!("{port} · {}", protocol.version)),
                };
                ui.label(RichText::new(text).color(p.text_dim));
                ui.label(RichText::new("●").color(color));
            });
        });
        ui.add_space(4.0);

        let log_height = 84.0;
        let status_height = 56.0;
        let available = ui.available_size();
        let side = available.x.min(available.y - log_height - status_height - MODE_STRIP_HEIGHT - 28.0).max(160.0);
        ui.vertical_centered(|ui| {
            // The work area sits straight on the window, outlined so that white stays visible.
            let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
            let radius = CornerRadius::same(16);
            egui::Image::new(&self.texture).corner_radius(radius).paint_at(ui, rect);
            ui.painter().rect_stroke(rect, radius, Stroke::new(1.0, theme::hairline(p)), egui::StrokeKind::Inside);
            self.paint_trace(ui, rect);
            self.preview_click(&response, rect);
            self.floating_overlay(ui, rect);
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mm = f64::from(self.base_size()) * MEASURED_MM_PER_PX;
            let burn = imaging::black_ratio(&self.plan().image) * 100.0;
            ui.label(theme::dim(format!(
                "{0}×{0} px   {1} {mm:.0}×{mm:.0} mm   {burn:.1}% {2}",
                self.base_size(),
                tr("about", "circa"),
                tr("to burn", "da bruciare")
            )));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let elapsed = self.engraving_since.map(|t| t.elapsed()).or(self.last_duration);
                if let Some(elapsed) = elapsed {
                    let secs = elapsed.as_secs();
                    ui.label(RichText::new(format!("{:02}:{:02}", secs / 60, secs % 60)).monospace());
                }
                if let Some((x, y)) = self.position {
                    ui.label(RichText::new(format!("X {x:3}  Y {y:3}")).monospace().color(p.text_dim));
                }
            });
        });
        self.estimate_line(ui);
        self.mode_strip(ui);
        self.log_view(ui, log_height);
    }

    /// The three print modes as pictures of the current image with their estimated time,
    /// and what the selected one does.
    fn mode_strip(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui);
        let frame = egui::Frame::new()
            .fill(p.surface)
            .corner_radius(CornerRadius::same(16))
            .inner_margin(egui::Margin::same(4));
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(MODE_STRIP_HEIGHT - 8.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let card = egui::vec2(MODE_STRIP_HEIGHT - 8.0 - 38.0, MODE_STRIP_HEIGHT - 8.0);
                for (i, kind) in ModeKind::ALL.into_iter().enumerate() {
                    let (rect, response) = ui.allocate_exact_size(card, egui::Sense::click());
                    let selected = kind == self.print_mode;
                    let radius = CornerRadius::same(12);
                    let fill = if selected {
                        p.bg
                    } else if response.hovered() {
                        p.track.gamma_multiply(0.5)
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, radius, fill);
                    if selected {
                        ui.painter().rect_stroke(rect, radius, Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
                    }
                    let side = card.x - 12.0;
                    let picture = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 6.0), egui::vec2(side, side));
                    egui::Image::new(&self.thumbnails[i]).corner_radius(CornerRadius::same(8)).paint_at(ui, picture);
                    ui.painter().rect_stroke(
                        picture,
                        CornerRadius::same(8),
                        Stroke::new(1.0, theme::hairline(p)),
                        egui::StrokeKind::Inside,
                    );
                    let plan = &self.plans[i];
                    let name_pos = egui::pos2(rect.center().x, picture.bottom() + 5.0);
                    let name_font = egui::FontId::proportional(12.5);
                    ui.painter().text(name_pos, egui::Align2::CENTER_TOP, kind.label(), name_font, p.text);
                    let time = if plan.is_empty() { "–:––".to_owned() } else { plan::clock(plan.seconds) };
                    ui.painter().text(
                        name_pos + egui::vec2(0.0, 16.0),
                        egui::Align2::CENTER_TOP,
                        time,
                        egui::FontId::monospace(12.5),
                        if selected { p.accent } else { p.text_dim },
                    );
                    let response = response.on_hover_text(mode_description(kind.mode(self.stroke)));
                    if response.clicked() && !selected {
                        self.print_mode = kind;
                        self.show_selected_plan();
                    }
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 8.0);
                    ui.add_space(8.0);
                    ui.label(RichText::new(tr("Print mode", "Modalità di stampa")).color(p.text_dim).size(12.5));
                    ui.add(egui::Label::new(RichText::new(mode_description(self.plan().mode)).size(12.5)).wrap());
                    if self.print_mode == ModeKind::Line {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr("Stroke (pixels)", "Tratto (pixel)")).size(12.5));
                            if theme::slider(ui, &mut self.stroke, 1, plan::MAX_STROKE, 110.0).changed() {
                                self.plans_dirty = true;
                            }
                        });
                    }
                });
            });
        });
        ui.add_space(6.0);
    }

    /// Estimated time of the current plan next to the single job, and the key of the colours.
    fn estimate_line(&self, ui: &mut egui::Ui) {
        let p = palette(ui);
        let plan = self.plan();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if plan.is_empty() {
                ui.label(theme::dim(tr("Nothing to burn yet", "Ancora niente da bruciare")));
                return;
            }
            ui.label(RichText::new(tr("Estimated time", "Tempo stimato")).small().color(p.text_dim));
            ui.label(RichText::new(plan::clock(plan.seconds)).monospace());
            if plan.jobs.len() > 1 {
                ui.label(theme::dim(format!("{} {} {}", tr("in", "in"), plan.jobs.len(), tr("jobs", "lavori"))));
            }
            if plan.mode != PrintMode::Single {
                ui.label(theme::dim(format!(
                    "{} {}",
                    tr("single job", "lavoro unico"),
                    plan::clock(plan.single_seconds)
                )));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut key = |colour: Color32, text: &str| {
                    ui.label(theme::dim(text));
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, colour);
                    ui.add_space(6.0);
                };
                if plan.jobs.len() > 1 {
                    key(colour(plan::CUT), tr("cut between jobs", "taglio fra i lavori"));
                    key(colour(plan::SAVED), tr("no longer crossed", "non più attraversato"));
                }
                key(colour(plan::TRAVEL), tr("crossed empty", "attraversato a vuoto"));
            });
        });
    }

    /// The positions reported by the engraver, and the head, over the preview.
    fn paint_trace(&self, ui: &egui::Ui, rect: egui::Rect) {
        let scale = rect.width() / self.base_size() as f32;
        let to_screen = |(x, y): (i32, i32)| rect.min + egui::vec2((x as f32 + 0.5) * scale, (y as f32 + 0.5) * scale);
        let painter = ui.painter_at(rect);
        let trace = palette(ui).err;
        let dot = scale.max(1.0);
        // One mesh for all the dots: tens of thousands of separate shapes would cost a
        // tessellation each, at every frame of a long engraving.
        let mut mesh = egui::Mesh::default();
        for &point in &self.dots {
            mesh.add_colored_rect(egui::Rect::from_center_size(to_screen(point), egui::vec2(dot, dot)), trace);
        }
        painter.add(egui::Shape::mesh(mesh));
        if let Some(head) = self.position {
            painter.circle_stroke(to_screen(head), 6.0, Stroke::new(2.0, palette(ui).accent));
        }
    }

    /// The floating text over the preview: drag it to move it, drag a corner to resize it.
    fn floating_overlay(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let p = palette(ui);
        let area = self.base_size() as f32;
        let scale = rect.width() / area;
        let Some(float) = self.floating.as_mut() else { return };
        float.refresh(ui.ctx());
        if float.is_empty() {
            return;
        }
        let size = egui::vec2(float.stamp.width() as f32, float.stamp.height() as f32) * scale;
        let screen = egui::Rect::from_min_size(rect.min + float.top_left() * scale, size);
        let painter = ui.painter_at(rect);
        if let Some(texture) = &float.texture {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(texture.id(), screen, uv, p.accent);
        }
        painter.rect_stroke(screen, 0.0, Stroke::new(1.0, p.accent), egui::StrokeKind::Outside);

        // The body first: the handles, added later, sit on top of it. Both are clipped to
        // the preview so that no hidden drag area covers the widgets around it.
        let body = ui.interact(screen.intersect(rect), ui.scope_id().with("floating-body"), egui::Sense::drag());
        let cursor = if body.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab };
        let body = body.on_hover_cursor(cursor);
        if body.dragged() {
            float.centre =
                (float.centre + body.drag_delta() / scale).clamp(egui::pos2(0.0, 0.0), egui::pos2(area, area));
        }
        let centre = screen.center();
        let corners = [screen.left_top(), screen.right_top(), screen.left_bottom(), screen.right_bottom()];
        for (i, corner) in corners.into_iter().enumerate() {
            // Kept inside the preview, so that a text wider than it still has handles to grab.
            let corner = rect.shrink(HANDLE / 2.0).clamp(corner);
            let handle = egui::Rect::from_center_size(corner, egui::vec2(HANDLE, HANDLE));
            painter.rect_filled(handle, 2.0, p.accent);
            let cursor = if i == 0 || i == 3 { egui::CursorIcon::ResizeNwSe } else { egui::CursorIcon::ResizeNeSw };
            let response = ui
                .interact(handle, ui.scope_id().with(("floating-corner", i)), egui::Sense::drag())
                .on_hover_cursor(cursor);
            if let Some(pointer) = response.interact_pointer_pos() {
                let distance = (pointer - centre).length().max(1.0);
                if response.drag_started() {
                    float.resize_from = Some((float.size, distance));
                    float.asked_mm = None;
                }
                if response.dragged()
                    && let Some((from_size, from_distance)) = float.resize_from
                {
                    float.set_size((from_size as f32 * distance / from_distance).round() as u32);
                }
            }
            if response.drag_stopped() {
                float.resize_from = None;
            }
        }
    }

    fn preview_click(&mut self, response: &egui::Response, rect: egui::Rect) {
        if !response.clicked() || self.floating.is_some() {
            return;
        }
        let Some(pos) = response.interact_pointer_pos() else { return };
        let scale = self.base_size() as f32 / rect.width();
        let (x, y) = (((pos.x - rect.left()) * scale) as i64, ((pos.y - rect.top()) * scale) as i64);
        if self.protocol().is_some_and(|p| p.version != Version::V10) {
            // The newer protocols can send the head to a point of the image.
            self.send(Command::MoveTo { x: x.clamp(0, 25_000) as u16, y: y.clamp(0, 25_000) as u16 });
        }
    }

    fn log_view(&self, ui: &mut egui::Ui, height: f32) {
        let p = palette(ui);
        egui::Frame::new()
            .fill(p.surface)
            .corner_radius(CornerRadius::same(16))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(height - 16.0);
                egui::ScrollArea::vertical().auto_shrink([false; 2]).stick_to_bottom(true).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    if self.log.is_empty() {
                        ui.label(theme::dim(tr(
                            "Open a picture or drop it here, then connect the engraver.",
                            "Apri un'immagine o trascinala qui, poi collega l'incisore.",
                        )));
                    }
                    for line in &self.log {
                        ui.label(RichText::new(line).small().color(p.text_dim));
                    }
                });
            });
    }

    // ---- controls --------------------------------------------------------------------

    /// One card per thing the system lacks, with the button that provides it.
    fn banner(&mut self, ui: &mut egui::Ui) {
        let p = palette(ui);
        let ctx = ui.ctx().clone();
        for problem in self.problems.clone() {
            let width = ui.available_width();
            egui::Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0, p.warn))
                .corner_radius(CornerRadius::same(16))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(width - 26.0);
                    ui.add(egui::Label::new(RichText::new(problem_title(&problem)).color(p.warn).strong()).wrap());
                    ui.add(egui::Label::new(theme::dim(problem_text(&problem))).wrap());
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        if self.fixing {
                            ui.spinner();
                            ui.label(theme::dim(tr(
                                "Waiting for the administrator password...",
                                "In attesa della password di amministratore...",
                            )));
                            return;
                        }
                        if problem.fix().is_some() && ui.add(theme::primary(ui, problem_fix_label(&problem))).clicked()
                        {
                            self.fix(&ctx, &problem);
                        }
                        if ui.button(tr("Check again", "Controlla di nuovo")).clicked() {
                            self.refresh_ports();
                        }
                    });
                });
            ui.add_space(8.0);
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        self.banner(ui);
        self.group_engraver(ui);
        self.group_image(ui);
        self.group_engraving(ui);
        self.group_head(ui);
        self.group_settings(ui);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(theme::dim(format!("neje {}", env!("CARGO_PKG_VERSION"))));
        });
    }

    fn group_engraver(&mut self, ui: &mut egui::Ui) {
        let busy = self.link != Link::Disconnected;
        theme::group(ui, tr("Engraver", "Incisore"), |ui| {
            theme::row(ui, tr("Port", "Porta"), |ui| {
                let mut options = vec![(None, tr("Automatic", "Automatica").to_owned())];
                options.extend(self.ports.iter().map(|port| (Some(port.name.clone()), port.name.clone())));
                if let Some(port) = self.port_choice.clone().filter(|c| !self.ports.iter().any(|p| &p.name == c)) {
                    options.push((Some(port.clone()), port));
                }
                ui.add_enabled_ui(!busy, |ui| {
                    if let Some(choice) = theme::choice(ui, "port", &self.port_choice, &options) {
                        self.port_choice = choice;
                    }
                    if theme::refresh_button(ui)
                        .on_hover_text(tr("Look for ports again", "Cerca di nuovo le porte"))
                        .clicked()
                    {
                        self.refresh_ports();
                    }
                });
            });
            theme::row(ui, tr("Protocol", "Protocollo"), |ui| {
                let options: Vec<_> = ProtocolChoice::ALL.iter().map(|&c| (c, c.label())).collect();
                ui.add_enabled_ui(!busy, |ui| {
                    if let Some(choice) = theme::choice(ui, "protocol", &self.protocol_choice, &options) {
                        self.protocol_choice = choice;
                    }
                });
            });
            let status = match &self.link {
                Link::Disconnected => tr("Not connected", "Non connesso"),
                Link::Connecting => tr("Searching...", "Ricerca..."),
                Link::Connected { .. } => tr("Connected", "Connesso"),
            };
            theme::row(ui, status, |ui| match self.link {
                Link::Disconnected => {
                    if ui.button(tr("Connect", "Connetti")).clicked() {
                        self.connect();
                    }
                }
                Link::Connecting => {
                    ui.spinner();
                }
                Link::Connected { .. } => {
                    if ui.button(tr("Disconnect", "Disconnetti")).clicked() {
                        self.worker.request(Request::Disconnect);
                    }
                }
            });
        });
    }

    fn group_image(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        theme::group(ui, tr("Image", "Immagine"), |ui| {
            let open =
                [(tr("Open picture…", "Apri immagine…"), true, ""), (tr("Open G-code…", "Apri G-code…"), true, "")];
            match theme::button_row(ui, &open) {
                Some(0) => self.pick(&ctx, Picked::Image, || {
                    rfd::FileDialog::new().add_filter(tr("Pictures", "Immagini"), &IMAGE_EXTENSIONS).pick_file()
                }),
                Some(_) => self.pick(&ctx, Picked::Gcode, || {
                    rfd::FileDialog::new().add_filter("G-code", &gcode::EXTENSIONS).pick_file()
                }),
                None => {}
            }
            theme::row(ui, tr("Conversion", "Conversione"), |ui| {
                let options = [
                    (Mode::Bw, tr("Black and white", "Bianco e nero").to_owned()),
                    (Mode::Dither, tr("Dithering (photos)", "Retinatura (foto)").to_owned()),
                    (Mode::GrayThreshold, tr("Fixed threshold", "Soglia fissa").to_owned()),
                ];
                if let Some(mode) = theme::choice(ui, "mode", &self.mode, &options) {
                    self.mode = mode;
                    if self.original.is_some() {
                        self.reapply();
                    }
                }
            });
            theme::row(ui, tr("Fit G-code to the area", "Adatta il G-code all'area"), |ui| {
                theme::toggle(ui, &mut self.gcode_fit);
            });
            let has_picture = self.original.is_some();
            let edit = [
                (tr("Rotate", "Ruota"), true, tr("Rotate 90° clockwise", "Ruota di 90° in senso orario")),
                (tr("Shrink", "Riduci"), true, tr("Shrink to 90%", "Riduci al 90%")),
                (
                    tr("Reload", "Ricarica"),
                    has_picture,
                    tr("Convert the opened picture again", "Converti di nuovo l'immagine aperta"),
                ),
                (tr("Clear", "Pulisci"), true, tr("Empty the work area", "Svuota l'area di lavoro")),
            ];
            match theme::button_row(ui, &edit) {
                Some(0) => self.set_base(imaging::rotate90(&self.base)),
                Some(1) => self.set_base(imaging::shrink(&self.base)),
                Some(2) => self.reapply(),
                Some(_) => {
                    self.original = None;
                    self.floating = None;
                    self.reapply();
                }
                None => {}
            }
            let max_mm = px_to_mm(self.base_size());
            let shown = self.size_mm.or_else(|| self.ink.map(|(_, _, w, h)| (px_to_mm(w), px_to_mm(h))));
            let mut change = None;
            theme::row(ui, tr("Size", "Dimensione"), |ui| {
                let Some((mut w, mut h)) = shown else {
                    ui.label(theme::dim(tr("nothing to burn", "niente da bruciare")));
                    return;
                };
                // Right to left: height first.
                let height = ui
                    .add(mm_field(&mut h, max_mm))
                    .on_hover_text(tr("Height of what will be burnt", "Altezza di ciò che verrà bruciato"));
                if height.changed() {
                    change = Some((h, false));
                }
                ui.label("×");
                let width = ui
                    .add(mm_field(&mut w, max_mm))
                    .on_hover_text(tr("Width of what will be burnt", "Larghezza di ciò che verrà bruciato"));
                if width.changed() {
                    change = Some((w, true));
                }
            });
            if let Some((mm, width)) = change {
                self.scale_to(mm, width);
            }
            let extra = [
                (tr("Add text…", "Aggiungi testo…"), self.floating.is_none(), ""),
                (tr("Save preview…", "Salva anteprima…"), true, ""),
            ];
            match theme::button_row(ui, &extra) {
                Some(0) => self.open_text(),
                Some(_) => self.pick(&ctx, Picked::SavePreview, || {
                    rfd::FileDialog::new().add_filter("PNG", &["png"]).set_file_name("neje-preview.png").save_file()
                }),
                None => {}
            }
        });
    }

    fn group_engraving(&mut self, ui: &mut egui::Ui) {
        let protocol = self.protocol();
        let connected = protocol.is_some();
        let idle = connected && self.upload.is_none();
        // Nothing is sent while the plan on screen is being recomputed.
        let floating = self.floating.is_some();
        let ready = idle && self.plans_ready() && !floating;
        let need = tr("Connect the engraver first", "Prima collega l'incisore");
        let place_first = tr("Place or cancel the text first", "Prima posiziona o annulla il testo");
        let can = |command: Command| protocol.is_some_and(|p| p.encode(command).is_ok());
        theme::group(ui, tr("Engraving", "Incisione"), |ui| {
            theme::row(ui, tr("Burn time", "Tempo di bruciatura"), |ui| {
                let width = (ui.available_width() - 68.0).clamp(60.0, 170.0);
                let response = theme::slider(ui, &mut self.burn, BURN_MIN, BURN_MAX, width);
                if response.changed() {
                    self.plans_dirty = true; // the estimates and the best cuts depend on it
                }
                // Like the original: the engraver gets the value when the slider is released.
                if (response.drag_stopped() || (response.changed() && !response.dragged())) && connected {
                    self.worker.request(Request::Send(Command::SetBurn(self.burn)));
                }
            });
            let jobs = self.plan().jobs.len();
            let empty = self.plan().is_empty();
            let sequence = self.sequence;
            let sequence_label = sequence.map(|(index, total)| format!("{} {index}/{total}", tr("Job", "Lavoro")));
            theme::row(ui, sequence_label.as_deref().unwrap_or(""), |ui| {
                let width = ui.available_width() - 8.0;
                if let Some(fraction) = self.upload {
                    ui.add(egui::ProgressBar::new(fraction).desired_width(width).show_percentage());
                    return;
                }
                if sequence.is_some() {
                    ui.label(theme::dim(tr("engraving", "incisione in corso")));
                    return;
                }
                let nothing = tr("Open a picture first", "Prima apri un'immagine");
                if jobs > 1 {
                    // One button for the whole sequence: upload, engrave, next job.
                    let label = format!("{} {jobs} {}", tr("Engrave in", "Incidi in"), tr("jobs", "lavori"));
                    let button = theme::primary(ui, &label).min_size(egui::vec2(width, theme::CONTROL_HEIGHT));
                    if ui
                        .add_enabled(ready, button)
                        .on_disabled_hover_text(if floating { place_first } else { need })
                        .clicked()
                    {
                        self.confirm = Some(LaserAction::Jobs);
                    }
                    return;
                }
                let label = if self.image_sent {
                    tr("Send the image again", "Invia di nuovo l'immagine")
                } else {
                    tr("Send the image to the engraver", "Invia l'immagine all'incisore")
                };
                // The next step of the job is the one accent button of the window.
                let button = if self.image_sent { egui::Button::new(label) } else { theme::primary(ui, label) };
                let response =
                    ui.add_enabled(ready && !empty, button.min_size(egui::vec2(width, theme::CONTROL_HEIGHT)));
                if response
                    .on_disabled_hover_text(if floating {
                        place_first
                    } else if connected {
                        nothing
                    } else {
                        need
                    })
                    .clicked()
                {
                    let image = self.plan().jobs[0].image.clone();
                    self.upload = Some(0.0);
                    self.uploading = Some(image.clone());
                    self.log(tr("Erasing the memory...", "Cancellazione della memoria..."));
                    self.worker.request(Request::Upload(image));
                }
            });
            theme::row(ui, "", |ui| {
                let width = ui.available_width() - 8.0;
                let label = tr("Start engraving", "Avvia l'incisione");
                let button = if self.image_sent { theme::primary(ui, label) } else { egui::Button::new(label) };
                let response = ui.add_enabled(idle, button.min_size(egui::vec2(width, theme::CONTROL_HEIGHT)));
                if response.on_disabled_hover_text(need).clicked() {
                    self.confirm = Some(LaserAction::Start);
                }
            });
            let control = [
                (tr("Pause", "Pausa"), connected, if connected { "" } else { need }),
                (
                    tr("Restart", "Riavvia"),
                    connected,
                    tr("Restart the engraver (Reboot)", "Riavvia l'incisore (Reboot)"),
                ),
            ];
            match theme::button_row(ui, &control) {
                Some(0) => self.send(Command::Pause),
                Some(_) => self.send(Command::Reset),
                None => {}
            }
            let only_some = tr("Not available with this protocol", "Non disponibile con questo protocollo");
            let hint = |command| {
                if !connected {
                    need
                } else if can(command) {
                    ""
                } else {
                    only_some
                }
            };
            let navigation = [
                (tr("◀◀ Back", "◀◀ Indietro"), idle && can(Command::Backward), hint(Command::Backward)),
                (tr("Redo", "Rifai"), idle && can(Command::Redo), hint(Command::Redo)),
                (tr("Forward ▶▶", "Avanti ▶▶"), idle && can(Command::Forward), hint(Command::Forward)),
            ];
            match theme::button_row(ui, &navigation) {
                Some(0) => self.send(Command::Backward),
                Some(1) => self.send(Command::Redo),
                Some(_) => self.send(Command::Forward),
                None => {}
            }
        });
        if self.engraving_since.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
    }

    fn group_head(&mut self, ui: &mut egui::Ui) {
        let protocol = self.protocol();
        let idle = protocol.is_some() && self.upload.is_none();
        let need = tr("Connect the engraver first", "Prima collega l'incisore");
        let can = |command: Command| protocol.is_some_and(|p| p.encode(command).is_ok());
        let tip = |text: &'static str| if protocol.is_some() { text } else { need };
        // The arrows move the reference of the image, not just the head: measured on the V10.
        theme::group(ui, tr("Position of the image", "Posizione dell'immagine"), |ui| {
            let arrows = [
                ("◀", idle, tip(tr("Shift the image 8 pixels to the left", "Sposta l'immagine di 8 pixel a sinistra"))),
                (
                    "▲",
                    idle,
                    tip(tr("Shift the image 8 pixels away from you", "Sposta l'immagine di 8 pixel verso il fondo")),
                ),
                (
                    "▼",
                    idle,
                    tip(tr(
                        "Shift the image 8 pixels towards you (on the DK-8-KZ this step often stalls)",
                        "Sposta l'immagine di 8 pixel verso di te (sulla DK-8-KZ questo passo spesso si blocca)",
                    )),
                ),
                ("▶", idle, tip(tr("Shift the image 8 pixels to the right", "Sposta l'immagine di 8 pixel a destra"))),
            ];
            let directions = [Direction::Left, Direction::Up, Direction::Down, Direction::Right];
            if let Some(i) = theme::button_row(ui, &arrows) {
                self.send(Command::Move(directions[i]));
            }
            let places = [
                (
                    tr("Centre", "Centro"),
                    idle,
                    tip(tr(
                        "Go to the centre of the image in memory (also stops the outline)",
                        "Vai al centro dell'immagine in memoria (ferma anche il riquadro)",
                    )),
                ),
                (
                    tr("Origin", "Origine"),
                    idle && can(Command::Home),
                    tip(tr("Go to the origin of the work area (V10)", "Vai all'origine dell'area di lavoro (V10)")),
                ),
                (
                    tr("Outline", "Riquadro"),
                    idle,
                    tip(tr(
                        "Trace the outline of the image over and over, laser on, until you press Centre",
                        "Traccia il contorno dell'immagine a ripetizione, laser acceso, finché non premi Centro",
                    )),
                ),
            ];
            match theme::button_row(ui, &places) {
                Some(0) => self.send(Command::Center),
                Some(1) => self.send(Command::Home),
                Some(_) => self.send(Command::Box),
                None => {}
            }
        });
    }

    fn group_settings(&mut self, ui: &mut egui::Ui) {
        theme::group(ui, tr("Application", "Applicazione"), |ui| {
            theme::row(ui, tr("Language", "Lingua"), |ui| {
                let options: Vec<_> = Lang::ALL.iter().map(|&l| (l, l.label().to_owned())).collect();
                if let Some(lang) = theme::choice(ui, "language", &i18n::current(), &options) {
                    i18n::set(lang);
                    self.settings.language = Some(lang.code().to_owned());
                    self.save_settings();
                }
            });
        });
    }

    fn save_settings(&mut self) {
        if self.shots.is_some() {
            return; // screenshots must not touch the user's settings
        }
        if let Err(e) = self.settings.save() {
            self.log_error(e);
        }
    }

    // ---- dialogs ---------------------------------------------------------------------

    fn laser_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.confirm else { return };
        let mut decision = None;
        theme::modal("laser", ctx).show(ctx, |ui| {
            let p = palette(ui);
            ui.set_width(380.0);
            ui.label(RichText::new(tr("The laser is about to switch on", "Il laser sta per accendersi")).font(theme::title_font(22.0)));
            ui.label(match action {
                LaserAction::Start if self.image_sent => tr(
                    "The engraver will burn the image you sent, at full power.",
                    "L'incisore brucerà l'immagine che hai inviato, a piena potenza.",
                ),
                LaserAction::Start => tr(
                    "No image was sent in this session: the engraver will burn whatever is left in its memory, at full power.",
                    "In questa sessione non è stata inviata nessuna immagine: l'incisore brucerà quella rimasta nella sua memoria, a piena potenza.",
                ),
                LaserAction::Jobs => tr(
                    "The image will be sent and engraved in several jobs, one after the other, at full power. Do not restart or switch off the engraver until the last one has finished, or the parts will not line up.",
                    "L'immagine verrà inviata e incisa in più lavori, uno dopo l'altro, a piena potenza. Non riavviare né spegnere l'incisore finché l'ultimo non è finito, altrimenti le parti non combaciano.",
                ),
                LaserAction::Command(_) => tr("This command moves the head with the laser on.", "Questo comando muove la testa con il laser acceso."),
            });
            ui.add_space(4.0);
            for item in laser_checklist() {
                ui.label(RichText::new(format!("•  {item}")).color(p.text_dim));
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let go = match action {
                    LaserAction::Start | LaserAction::Jobs => tr("Start engraving", "Avvia l'incisione"),
                    LaserAction::Command(_) => tr("Switch the laser on", "Accendi il laser"),
                };
                if ui.add(theme::primary(ui, go)).clicked() {
                    decision = Some(true);
                }
                if ui.button(tr("Cancel", "Annulla")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => {
                self.confirm = None;
                match action {
                    LaserAction::Start => {
                        self.settings.burn = self.burn;
                        self.save_settings();
                        self.worker.request(Request::Start(self.burn));
                    }
                    LaserAction::Jobs => {
                        self.settings.burn = self.burn;
                        self.save_settings();
                        let images: Vec<GrayImage> = self.plan().jobs.iter().map(|job| job.image.clone()).collect();
                        self.sequence = Some((1, images.len()));
                        self.worker.request(Request::RunJobs { images, burn: self.burn });
                    }
                    LaserAction::Command(command) => self.worker.request(Request::Send(command)),
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    /// The window of the floating text: what it says, its font and its height.
    /// `laser_open` tells whether the laser confirmation was open in this frame: Esc and
    /// Ctrl+Enter then belong to it.
    fn text_window(&mut self, ctx: &egui::Context, laser_open: bool) {
        let max_mm = px_to_mm(self.base_size());
        let Some(float) = self.floating.as_mut() else { return };
        let (fonts, scanning): (&[FontEntry], bool) = match &self.fonts {
            Fonts::Ready(list) => (list, false),
            Fonts::Scanning(_) => (&[], true),
        };
        let mut decision = None;
        let mut chosen = None;
        let mut asked = None;
        egui::Window::new(tr("Text to engrave", "Testo da incidere"))
            .id(egui::Id::unique("text-window"))
            .frame(theme::panel_frame(ctx))
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .default_pos(egui::pos2(48.0, 96.0))
            .show(ctx, |ui| {
                let edit = egui::TextEdit::multiline(&mut float.text)
                    .hint_text(tr("Type the text here", "Scrivi qui il testo"))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY);
                if ui.add(edit).changed() {
                    float.dirty = true;
                    float.fit_height();
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(tr("Font", "Carattere"));
                    ui.label(RichText::new(font_label(&float.font, fonts)).strong());
                });
                let search = egui::TextEdit::singleline(&mut float.filter)
                    .hint_text(tr("Search fonts", "Cerca un carattere"))
                    .desired_width(f32::INFINITY);
                ui.add(search);
                let needle = float.filter.to_lowercase();
                let built_in = tr("Inter (built-in)", "Inter (integrato)");
                let show_built_in = built_in.to_lowercase().contains(&needle);
                let matches: Vec<&FontEntry> =
                    fonts.iter().filter(|f| f.label().to_lowercase().contains(&needle)).collect();
                let first = usize::from(show_built_in);
                let row_height = ui.spacing().interact_size.y;
                egui::ScrollArea::vertical().max_height(160.0).auto_shrink([false, true]).show_rows(
                    ui,
                    row_height,
                    matches.len() + first,
                    |ui, rows| {
                        for row in rows {
                            let (source, label) = if row < first {
                                (FontSource::BuiltIn, built_in.to_owned())
                            } else {
                                let entry = matches[row - first];
                                (entry.source(), entry.label())
                            };
                            if ui.selectable_label(source == float.font, label).clicked() {
                                chosen = Some(source);
                            }
                        }
                    },
                );
                if scanning {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(theme::dim(tr("Looking for fonts...", "Ricerca dei caratteri...")));
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(tr("Height", "Altezza"));
                    // The ink, as the Size row measures it, not the white border of the stamp.
                    let (_, _, iw, ih) =
                        imaging::ink_bounds(&float.stamp).unwrap_or((0, 0, float.stamp.width(), float.stamp.height()));
                    let measured = px_to_mm(ih);
                    let mut mm = float.asked_mm.unwrap_or(measured);
                    if ui.add_enabled(!float.is_empty(), mm_field(&mut mm, max_mm)).changed() {
                        asked = Some(mm);
                    }
                    if !float.is_empty() {
                        let w = px_to_mm(iw);
                        ui.label(theme::dim(format!("{w:.1} × {measured:.1} mm")));
                    }
                });
                ui.label(theme::dim(tr(
                    "Drag the text on the preview to move it, drag a corner to resize it.",
                    "Trascina il testo sull'anteprima per spostarlo, trascina un angolo per ridimensionarlo.",
                )));
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let place = ui
                        .add_enabled(!float.is_empty(), theme::primary(ui, tr("Place", "Posiziona")))
                        .on_hover_text(tr("Burn it into the image (Ctrl+Enter)", "Fissalo nell'immagine (Ctrl+Invio)"));
                    if place.clicked() {
                        decision = Some(true);
                    }
                    if ui.button(tr("Cancel", "Annulla")).clicked() {
                        decision = Some(false);
                    }
                });
            });
        if let Some(mm) = asked {
            float.asked_mm = Some(mm);
            float.fit_height();
        }
        let mut error = None;
        if let Some(source) = chosen {
            match text::load(&source) {
                Ok(face) => {
                    float.font = source;
                    float.face = face;
                    float.dirty = true;
                    float.fit_height();
                }
                Err(e) => error = Some(e),
            }
        }
        let empty = float.is_empty();
        if let Some(e) = error {
            self.log_error(e);
        }
        let (escape, ctrl_enter) =
            ctx.input(|i| (i.key_pressed(egui::Key::Escape), i.modifiers.command && i.key_pressed(egui::Key::Enter)));
        // The first Esc only leaves the field being edited; the next one cancels.
        let escape = escape && !self.had_focus;
        if !laser_open {
            if escape {
                decision = Some(false);
            } else if ctrl_enter && !empty {
                decision = Some(true);
            }
        }
        match decision {
            Some(true) => self.place_text(),
            Some(false) => self.floating = None,
            None => {}
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_worker();
        self.poll_files(&ctx);
        self.poll_fonts();
        self.refresh_plans();
        self.screenshot_step(&ctx);
        let p = palette(ui);
        egui::Panel::right("controls")
            .resizable(false)
            .exact_size(PANEL_WIDTH)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 4, right: 12, top: 16, bottom: 8 }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| self.controls(ui));
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 16, right: 8, top: 12, bottom: 12 }))
            .show(ui, |ui| self.preview(ui));
        let laser_open = self.confirm.is_some();
        self.laser_dialog(&ctx);
        self.text_window(&ctx, laser_open);
        self.had_focus = ctx.memory(|m| m.focused().is_some());
    }
}
