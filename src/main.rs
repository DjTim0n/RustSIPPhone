// В релизной сборке на Windows не показываем чёрную консоль рядом с окном.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod audio;
mod call;
mod engine;
mod g711;
mod media;
mod model;
mod net;
mod ringtone;
mod rtp;
mod sdp;
mod store;
mod ui;

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("встроенная иконка повреждена");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Телефон")
            .with_icon(icon)
            .with_inner_size([400.0, 720.0])
            .with_min_inner_size([360.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Телефон",
        options,
        Box::new(|cc| Ok(Box::new(ui::PhoneApp::new(cc)))),
    )
}
