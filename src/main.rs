// In release builds on Windows, do not show a black console window next to the app.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod audio;
mod call;
mod engine;
mod g711;
mod i18n;
mod media;
mod model;
mod net;
mod ringtone;
mod rtp;
mod sdp;
mod store;
mod tray;
mod ui;
mod window;

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("the embedded icon is corrupt");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("RustSIPPhone")
            .with_icon(icon)
            // A fixed-size window: it cannot be resized or maximized.
            .with_inner_size(window::WINDOW_SIZE)
            .with_min_inner_size(window::WINDOW_SIZE)
            .with_max_inner_size(window::WINDOW_SIZE)
            .with_resizable(false)
            .with_maximize_button(false),
        ..Default::default()
    };
    eframe::run_native(
        "RustSIPPhone",
        options,
        Box::new(|cc| Ok(Box::new(ui::PhoneApp::new(cc)))),
    )
}
