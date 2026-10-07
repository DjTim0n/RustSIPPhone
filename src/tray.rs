//! System tray icon (Windows).
//!
//! On Windows the way to "run in the background" is the notification area: closing the window
//! hides it there, the phone keeps running, and the icon brings the window back (a click, or the
//! menu) - and so does an incoming call. On macOS the Dock plays this role and on Linux the
//! window is minimized, so there this module is an inert stub.

use crate::i18n::Lang;
use eframe::egui;

/// What the user asked for through the tray icon.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum TrayAction {
    /// Show the window again.
    Show,
    /// Quit the app for real.
    Quit,
}

#[cfg(windows)]
pub use windows_tray::Tray;

#[cfg(not(windows))]
pub use stub::Tray;

#[cfg(windows)]
mod windows_tray {
    use super::*;
    use std::sync::mpsc::{Receiver, channel};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    pub struct Tray {
        // Dropping the icon removes it from the tray, so it has to be kept alive.
        _icon: TrayIcon,
        show: MenuItem,
        quit: MenuItem,
        actions: Receiver<TrayAction>,
    }

    impl Tray {
        /// Creates the tray icon. Must run on the thread that runs the event loop.
        /// Returns `None` if the icon could not be created; the app then falls back to minimizing.
        pub fn new(ctx: &egui::Context, lang: Lang) -> Option<Tray> {
            let image =
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/tray.png")).ok()?;
            let icon = Icon::from_rgba(image.rgba, image.width, image.height).ok()?;

            let show = MenuItem::new(show_label(lang), true, None);
            let quit = MenuItem::new(quit_label(lang), true, None);
            let menu = Menu::new();
            menu.append(&show).ok()?;
            menu.append(&quit).ok()?;

            let (sender, actions) = channel();

            // Menu clicks. The handler runs on the event-loop thread, so it only forwards the
            // action and wakes the UI up, which then handles it in `App::logic`.
            let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
            let (menu_sender, menu_ctx) = (sender.clone(), ctx.clone());
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let action = if event.id == show_id {
                    Some(TrayAction::Show)
                } else if event.id == quit_id {
                    Some(TrayAction::Quit)
                } else {
                    None
                };
                if let Some(action) = action {
                    let _ = menu_sender.send(action);
                    menu_ctx.request_repaint();
                }
            }));

            // A left click (or double click) on the icon itself shows the window; the menu opens
            // on a right click.
            let click_ctx = ctx.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                let wants_window = matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                );
                if wants_window {
                    let _ = sender.send(TrayAction::Show);
                    click_ctx.request_repaint();
                }
            }));

            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("RustSIPPhone")
                .with_icon(icon)
                .build()
                .ok()?;

            Some(Tray {
                _icon: icon,
                show,
                quit,
                actions,
            })
        }

        /// Everything the user asked for since the last call.
        pub fn actions(&self) -> Vec<TrayAction> {
            self.actions.try_iter().collect()
        }

        pub fn set_language(&self, lang: Lang) {
            self.show.set_text(show_label(lang));
            self.quit.set_text(quit_label(lang));
        }
    }

    fn show_label(lang: Lang) -> &'static str {
        lang.t("Show RustSIPPhone", "Показать RustSIPPhone")
    }

    fn quit_label(lang: Lang) -> &'static str {
        lang.t("Quit", "Выйти")
    }
}

#[cfg(not(windows))]
mod stub {
    use super::*;

    pub struct Tray;

    #[allow(dead_code)]
    impl Tray {
        pub fn new(_ctx: &egui::Context, _lang: Lang) -> Option<Tray> {
            None
        }

        pub fn actions(&self) -> Vec<TrayAction> {
            Vec::new()
        }

        pub fn set_language(&self, _lang: Lang) {}
    }
}
