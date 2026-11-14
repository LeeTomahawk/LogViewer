// Release builds on Windows run without a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod filter_panel;
mod fmt;
mod fonts;
mod format_panel;
mod highlight_panel;
mod search;
mod selection;
mod tab;
mod view;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    let initial: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("logview")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0])
            .with_drag_and_drop(true)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/logview.png"))
                    .unwrap_or_default(),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "logview",
        options,
        Box::new(move |cc| Ok(Box::new(app::LogViewApp::new(cc, initial)))),
    )
}
