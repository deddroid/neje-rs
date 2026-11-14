//! The window: preview on the left, controls on the right. Everything that touches the
//! serial port goes through the [`Worker`] thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke};
use image::{GrayImage, RgbImage};
use neje::device::{self, PortInfo};
use neje::i18n::{self, Lang, tr};
use neje::imaging::{self, Mode};
use neje::messages::{describe, laser_checklist};
use neje::protocol::{BURN_MAX, BURN_MIN, Command, Direction, Event, Protocol, Version};
use neje::{gcode, text};

use crate::settings::Settings;
use crate::theme::{self, palette};
use crate::worker::{Request, Update, Worker};

const PANEL_WIDTH: f32 = 368.0;
const LOG_LINES: usize = 200;
/// Positions kept for the red trace on the preview.
const MAX_DOTS: usize = 40_000;
const GCODE_EXTENSIONS: [&str; 5] = ["nc", "gcode", "gc", "ngc", "tap"];
const IMAGE_EXTENSIONS: [&str; 8] = ["jpg", "jpeg", "png", "bmp", "gif", "tif", "tiff", "webp"];

/// What the window was asked to do at start-up.
#[derive(Default)]
pub struct Startup {
    pub port: Option<String>,
    pub protocol: Option<ProtocolChoice>,
    pub image: Option<PathBuf>,
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
    Command(Command),
}

struct TextDraft {
    text: String,
    size: u32,
}

enum Picked {
    Image(PathBuf),
    Gcode(PathBuf),
    SavePreview(PathBuf),
}

struct Screenshots {
    dir: PathBuf,
    /// Theme, language and whether the laser confirmation is open.
    steps: Vec<(egui::Theme, Lang, bool)>,
    index: usize,
    frames: u32,
    requested: bool,
    tx: Sender<Arc<egui::ColorImage>>,
    rx: Receiver<Arc<egui::ColorImage>>,
}

pub struct App {
    worker: Worker,
    settings: Settings,

    base: GrayImage,
    texture: egui::TextureHandle,
    /// The picture as opened, to convert it again when the mode changes.
    original: Option<RgbImage>,
    mode: Mode,
    gcode_fit: bool,
    /// Text waiting for a click on the preview to be placed.
    pending_text: Option<GrayImage>,
    text_dialog: Option<TextDraft>,

    link: Link,
    ports: Vec<PortInfo>,
    port_choice: Option<String>,
    protocol_choice: ProtocolChoice,

    burn: u8,
    /// Fraction sent, while an image is going to the engraver.
    upload: Option<f32>,
    /// True when the engraver holds the image shown in the preview.
    image_sent: bool,
    engraving_since: Option<Instant>,
    last_duration: Option<Duration>,
    position: Option<(i32, i32)>,
    dots: Vec<(i32, i32)>,
    confirm: Option<LaserAction>,

    log: Vec<String>,
    picked_tx: Sender<Picked>,
    picked_rx: Receiver<Picked>,
    shots: Option<Screenshots>,
}

fn texture_image(base: &GrayImage) -> egui::ColorImage {
    egui::ColorImage::from_gray([base.width() as usize, base.height() as usize], base.as_raw())
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        i18n::set(settings.lang());
        let base = imaging::new_base(Protocol::new(Version::V10, None).base_size());
        let texture = cc.egui_ctx.load_texture("preview", texture_image(&base), egui::TextureOptions::LINEAR);
        let (picked_tx, picked_rx) = mpsc::channel();
        let shots = startup.screenshots.map(|dir| {
            let (tx, rx) = mpsc::channel();
            let mut steps: Vec<_> = [egui::Theme::Dark, egui::Theme::Light]
                .into_iter()
                .flat_map(|theme| Lang::ALL.map(|lang| (theme, lang, false)))
                .collect();
            steps.extend([(egui::Theme::Dark, Lang::En, true), (egui::Theme::Light, Lang::It, true)]);
            Screenshots { dir, steps, index: 0, frames: 0, requested: false, tx, rx }
        });
        let mut app = App {
            worker: Worker::spawn(cc.egui_ctx.clone()),
            burn: settings.burn.clamp(BURN_MIN, BURN_MAX),
            settings,
            base,
            texture,
            original: None,
            mode: Mode::Bw,
            gcode_fit: true,
            pending_text: None,
            text_dialog: None,
            link: Link::Disconnected,
            ports: Vec::new(),
            port_choice: startup.port.clone(),
            protocol_choice: startup.protocol.unwrap_or(ProtocolChoice::V10),
            upload: None,
            image_sent: false,
            engraving_since: None,
            last_duration: None,
            position: None,
            dots: Vec::new(),
            confirm: None,
            log: Vec::new(),
            picked_tx,
            picked_rx,
            shots,
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

    fn set_base(&mut self, base: GrayImage) {
        self.base = base;
        self.texture.set(texture_image(&self.base), egui::TextureOptions::LINEAR);
        self.image_sent = false;
        self.dots.clear();
        self.position = None;
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
    }

    fn refresh_ports(&mut self) {
        match device::ports() {
            Ok(ports) => self.ports = ports,
            Err(e) => self.log_error(e),
        }
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
        if has_extension(path, &GCODE_EXTENSIONS) {
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
                    self.resize_base(protocol.base_size());
                }
                Update::Disconnected => {
                    if self.link != Link::Disconnected {
                        self.log(tr("Disconnected", "Disconnesso"));
                    }
                    self.link = Link::Disconnected;
                    self.upload = None;
                    self.image_sent = false;
                    self.stop_clock();
                }
                Update::Event(event) => self.on_event(event),
                Update::Sent(command) => self.log(format!("{}: {}", tr("Sent", "Inviato"), command.name())),
                Update::Started => {
                    self.engraving_since = Some(Instant::now());
                    self.last_duration = None;
                    self.dots.clear();
                    self.log(format!(
                        "{} {}",
                        tr("Engraving started, burn time", "Incisione avviata, tempo di bruciatura"),
                        self.burn
                    ));
                }
                Update::UploadProgress { sent, total } => self.upload = Some(sent as f32 / total.max(1) as f32),
                Update::UploadDone => {
                    self.upload = None;
                    self.image_sent = true;
                    self.log(tr("Image sent to the engraver", "Immagine inviata all'incisore"));
                }
                Update::Error(error) => {
                    self.upload = None;
                    if self.link == Link::Connecting {
                        self.link = Link::Disconnected;
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
                Picked::SavePreview(path) => match self.base.save(&path) {
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
        let connecting = self.link == Link::Connecting;
        let Some(s) = self.shots.as_mut() else { return };
        ctx.request_repaint();
        if connecting {
            return; // wait for the engraver, so the picture shows the connected state
        }
        if let Ok(image) = s.rx.try_recv() {
            let (theme, lang, dialog) = s.steps[s.index];
            let theme = if theme == egui::Theme::Dark { "dark" } else { "light" };
            let path = s.dir.join(format!("{}_{theme}{}.png", lang.code(), if dialog { "_laser" } else { "" }));
            let [w, h] = image.size;
            if let Err(e) = image::save_buffer(&path, image.as_raw(), w as u32, h as u32, image::ColorType::Rgba8) {
                eprintln!("{}: {e}", path.display());
            }
            s.index += 1;
            s.frames = 0;
            s.requested = false;
        }
        let Some(&(theme, lang, dialog)) = s.steps.get(s.index) else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        ctx.set_theme(theme);
        i18n::set(lang);
        self.confirm = dialog.then_some(LaserAction::Start);
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

        let log_height = 118.0;
        let status_height = 30.0;
        let available = ui.available_size();
        let side = available.x.min(available.y - log_height - status_height - 16.0).max(160.0);
        ui.vertical_centered(|ui| {
            // The work area sits straight on the window, outlined so that white stays visible.
            let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
            let radius = CornerRadius::same(16);
            egui::Image::new(&self.texture).corner_radius(radius).paint_at(ui, rect);
            ui.painter().rect_stroke(rect, radius, Stroke::new(1.0, theme::hairline(p)), egui::StrokeKind::Inside);
            self.paint_trace(ui, rect);
            self.preview_click(&response, rect);
            if self.pending_text.is_some() {
                let hint = tr("Click where the text goes", "Clicca dove mettere il testo");
                let pos = rect.center_top() + egui::vec2(0.0, 22.0);
                let galley =
                    ui.painter().layout_no_wrap(hint.to_owned(), egui::FontId::proportional(14.0), Color32::WHITE);
                let bubble = egui::Rect::from_center_size(pos, galley.size() + egui::vec2(20.0, 10.0));
                ui.painter().rect_filled(bubble, bubble.height() / 2.0, p.accent);
                ui.painter().galley(bubble.center() - galley.size() / 2.0, galley, Color32::WHITE);
                response.on_hover_cursor(egui::CursorIcon::Crosshair);
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mm = f64::from(self.base_size()) * gcode::MM_PER_PX;
            let burn = imaging::black_ratio(&self.base) * 100.0;
            ui.label(theme::dim(format!(
                "{0}×{0} px   {mm:.2}×{mm:.2} mm   {burn:.1}% {1}",
                self.base_size(),
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
        self.log_view(ui, log_height);
    }

    /// The positions reported by the engraver, and the head, over the preview.
    fn paint_trace(&self, ui: &egui::Ui, rect: egui::Rect) {
        let scale = rect.width() / self.base_size() as f32;
        let to_screen = |(x, y): (i32, i32)| rect.min + egui::vec2((x as f32 + 0.5) * scale, (y as f32 + 0.5) * scale);
        let painter = ui.painter_at(rect);
        let trace = palette(ui).err;
        let dot = scale.max(1.0);
        for &point in &self.dots {
            painter.rect_filled(egui::Rect::from_center_size(to_screen(point), egui::vec2(dot, dot)), 0.0, trace);
        }
        if let Some(head) = self.position {
            painter.circle_stroke(to_screen(head), 6.0, Stroke::new(2.0, palette(ui).accent));
        }
    }

    fn preview_click(&mut self, response: &egui::Response, rect: egui::Rect) {
        if !response.clicked() {
            return;
        }
        let Some(pos) = response.interact_pointer_pos() else { return };
        let scale = self.base_size() as f32 / rect.width();
        let (x, y) = (((pos.x - rect.left()) * scale) as i64, ((pos.y - rect.top()) * scale) as i64);
        if let Some(stamp) = self.pending_text.take() {
            let base = imaging::place(&self.base, &stamp, x, y);
            self.set_base(base);
        } else if self.protocol().is_some_and(|p| p.version != Version::V10) {
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

    fn controls(&mut self, ui: &mut egui::Ui) {
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
                    if ui.button("⟳").on_hover_text(tr("Look for ports again", "Cerca di nuovo le porte")).clicked() {
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
                    rfd::FileDialog::new().add_filter("G-code", &GCODE_EXTENSIONS).pick_file()
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
                    self.pending_text = None;
                    self.reapply();
                }
                None => {}
            }
            let extra =
                [(tr("Add text…", "Aggiungi testo…"), true, ""), (tr("Save preview…", "Salva anteprima…"), true, "")];
            match theme::button_row(ui, &extra) {
                Some(0) => self.text_dialog = Some(TextDraft { text: String::new(), size: 48 }),
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
        let need = tr("Connect the engraver first", "Prima collega l'incisore");
        let can = |command: Command| protocol.is_some_and(|p| p.encode(command).is_ok());
        theme::group(ui, tr("Engraving", "Incisione"), |ui| {
            theme::row(ui, tr("Burn time", "Tempo di bruciatura"), |ui| {
                let width = (ui.available_width() - 68.0).clamp(60.0, 170.0);
                let response = theme::slider(ui, &mut self.burn, BURN_MIN, BURN_MAX, width);
                // Like the original: the engraver gets the value when the slider is released.
                if (response.drag_stopped() || (response.changed() && !response.dragged())) && connected {
                    self.worker.request(Request::Send(Command::SetBurn(self.burn)));
                }
            });
            theme::row(ui, "", |ui| {
                let width = ui.available_width() - 8.0;
                if let Some(fraction) = self.upload {
                    ui.add(egui::ProgressBar::new(fraction).desired_width(width).show_percentage());
                    return;
                }
                let label = if self.image_sent {
                    tr("Send the image again", "Invia di nuovo l'immagine")
                } else {
                    tr("Send the image to the engraver", "Invia l'immagine all'incisore")
                };
                // The next step of the job is the one accent button of the window.
                let button = if self.image_sent { egui::Button::new(label) } else { theme::primary(ui, label) };
                let response = ui.add_enabled(idle, button.min_size(egui::vec2(width, theme::CONTROL_HEIGHT)));
                if response.on_disabled_hover_text(need).clicked() {
                    self.upload = Some(0.0);
                    self.log(tr("Erasing the memory...", "Cancellazione della memoria..."));
                    self.worker.request(Request::Upload(self.base.clone()));
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
        theme::group(ui, tr("Head", "Testa"), |ui| {
            let arrows = [
                ("◀", idle, tip(tr("Left", "Sinistra"))),
                ("▲", idle, tip(tr("Up", "Su"))),
                ("▼", idle, tip(tr("Down", "Giù"))),
                ("▶", idle, tip(tr("Right", "Destra"))),
            ];
            let directions = [Direction::Left, Direction::Up, Direction::Down, Direction::Right];
            if let Some(i) = theme::button_row(ui, &arrows) {
                self.send(Command::Move(directions[i]));
            }
            let places = [
                (tr("Centre", "Centro"), idle, tip(tr("Go to the centre", "Vai al centro"))),
                (
                    tr("Origin", "Origine"),
                    idle && can(Command::Home),
                    tip(tr("Go to the origin (V10)", "Vai all'origine (V10)")),
                ),
                (
                    tr("Outline", "Riquadro"),
                    idle,
                    tip(tr(
                        "Trace the outline of the image (laser on)",
                        "Traccia il contorno dell'immagine (laser acceso)",
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
                LaserAction::Command(_) => tr("This command moves the head with the laser on.", "Questo comando muove la testa con il laser acceso."),
            });
            ui.add_space(4.0);
            for item in laser_checklist() {
                ui.label(RichText::new(format!("•  {item}")).color(p.text_dim));
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let go = match action {
                    LaserAction::Start => tr("Start engraving", "Avvia l'incisione"),
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
                    LaserAction::Command(command) => self.worker.request(Request::Send(command)),
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn text_dialog(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.text_dialog.as_mut() else { return };
        let mut decision = None;
        theme::modal("text", ctx).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(RichText::new(tr("Text to engrave", "Testo da incidere")).font(theme::title_font(22.0)));
            ui.add(egui::TextEdit::multiline(&mut draft.text).desired_rows(3).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                ui.label(tr("Size (pixels)", "Dimensione (pixel)"));
                ui.add(egui::DragValue::new(&mut draft.size).range(6..=400));
            });
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !draft.text.trim().is_empty();
                if ui
                    .add_enabled(ready, theme::primary(ui, tr("Place on the image", "Posiziona sull'immagine")))
                    .clicked()
                {
                    decision = Some(true);
                }
                if ui.button(tr("Cancel", "Annulla")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        if let Some(accepted) = decision {
            let draft = self.text_dialog.take().expect("the dialog is open");
            if accepted {
                match text::render(&draft.text, None, draft.size) {
                    Ok(stamp) => self.pending_text = Some(stamp),
                    Err(e) => self.log_error(e),
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_worker();
        self.poll_files(&ctx);
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
        self.laser_dialog(&ctx);
        self.text_dialog(&ctx);
    }
}
