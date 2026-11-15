//! Graphical interface: `neje-gui [--port PORT] [--protocol V10] [FILE]`.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod settings;
mod theme;
mod worker;

use std::path::PathBuf;

use clap::Parser;
use eframe::egui;

use app::{App, ModeKind, ProtocolChoice, Startup};

#[derive(Parser)]
#[command(name = "neje-gui", version, about = "Graphical interface for NEJE DK-8-KZ laser engravers")]
struct Args {
    /// Serial port to connect to at start-up
    #[arg(short, long)]
    port: Option<String>,
    /// Protocol of the engraver (default: V10)
    #[arg(long, value_enum, ignore_case = true)]
    protocol: Option<ProtocolChoice>,
    /// Print mode selected at start-up (default: single)
    #[arg(long, value_enum)]
    print_mode: Option<ModeKind>,
    /// Stroke width of the line mode, in pixels
    #[arg(long)]
    stroke: Option<u8>,
    /// Picture or G-code file to open
    file: Option<PathBuf>,
}

fn window_icon() -> Option<egui::IconData> {
    let icon = image::load_from_memory(include_bytes!("../../assets/icon-256.png")).ok()?.into_rgba8();
    Some(egui::IconData { width: icon.width(), height: icon.height(), rgba: icon.into_raw() })
}

fn main() -> eframe::Result {
    let args = Args::parse();
    let startup = Startup {
        port: args.port,
        protocol: args.protocol,
        image: args.file,
        print_mode: args.print_mode,
        stroke: args.stroke,
        // NEJE_GUI_SCREENSHOTS=<folder>: save one screenshot per theme and language, then quit.
        screenshots: std::env::var_os("NEJE_GUI_SCREENSHOTS").map(PathBuf::from),
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("NEJE")
        .with_app_id("neje")
        .with_inner_size([1100.0, 950.0])
        .with_min_inner_size([860.0, 620.0]);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("neje", options, Box::new(|cc| Ok(Box::new(App::new(cc, startup)))))
}
