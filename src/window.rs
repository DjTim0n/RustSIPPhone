//! Window behaviour: a fixed size, staying alive in the background and surfacing on incoming calls.
//!
//! A phone has to keep running (and stay registered) when its window is closed, otherwise it
//! cannot ring. So closing the window never quits the app, and an incoming call brings it back.
//!
//! * macOS: the window just goes away, as in any Mac app, while the app keeps running; clicking
//!   its Dock icon brings it back.
//! * Windows and Linux: the window is minimized, so the taskbar button stays as the way back in.

use eframe::egui::{UserAttentionType, ViewportCommand, WindowLevel};

/// The window cannot be resized, so this is its exact inner size.
pub const WINDOW_SIZE: [f32; 2] = [400.0, 720.0];

/// What to do when the user asks to close the window.
///
/// Normally the request is cancelled so the phone keeps running; on Windows and Linux the window
/// is also minimized (on macOS [`hide_application`] takes it out of sight). When the user chose
/// to quit, the request goes through and the app exits.
pub fn close_request_commands(quitting: bool) -> Vec<ViewportCommand> {
    if quitting {
        Vec::new()
    } else if cfg!(target_os = "macos") {
        vec![ViewportCommand::CancelClose]
    } else {
        vec![
            ViewportCommand::CancelClose,
            ViewportCommand::Minimized(true),
        ]
    }
}

/// Hides the application the way Cmd+H does: the window disappears, the app keeps running, and a
/// click on its Dock icon (or Cmd+Tab) brings the window back already active. Only used on macOS;
/// elsewhere the window is minimized by [`close_request_commands`] instead.
#[cfg(target_os = "macos")]
pub fn hide_application() {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;
    // Event handling runs on the main thread. Should that ever not hold, do nothing rather than panic.
    if let Some(main_thread) = MainThreadMarker::new() {
        NSApplication::sharedApplication(main_thread).hide(None);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn hide_application() {}

/// How to make an incoming call impossible to miss: restore the window, show it above other
/// windows, give it focus, and bounce the Dock icon / flash the taskbar button until it is seen.
pub fn incoming_call_commands() -> Vec<ViewportCommand> {
    vec![
        ViewportCommand::Minimized(false),
        ViewportCommand::Visible(true),
        ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
        ViewportCommand::Focus,
        ViewportCommand::RequestUserAttention(UserAttentionType::Critical),
    ]
}

/// Undo [`incoming_call_commands`] once the call is answered, declined or missed.
pub fn ring_finished_commands() -> Vec<ViewportCommand> {
    vec![
        ViewportCommand::WindowLevel(WindowLevel::Normal),
        ViewportCommand::RequestUserAttention(UserAttentionType::Reset),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_window_never_quits() {
        let commands = close_request_commands(false);
        assert!(commands.contains(&ViewportCommand::CancelClose));
    }

    #[test]
    fn only_windows_and_linux_minimize_on_close() {
        let minimizes = close_request_commands(false).contains(&ViewportCommand::Minimized(true));
        assert_eq!(minimizes, !cfg!(target_os = "macos"));
    }

    #[test]
    fn quitting_lets_the_close_request_through() {
        assert!(close_request_commands(true).is_empty());
    }

    #[test]
    fn incoming_call_restores_focuses_and_raises_the_window() {
        let commands = incoming_call_commands();
        assert!(commands.contains(&ViewportCommand::Minimized(false)));
        assert!(commands.contains(&ViewportCommand::Visible(true)));
        assert!(commands.contains(&ViewportCommand::Focus));
        assert!(commands.contains(&ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop)));
        assert!(commands.contains(&ViewportCommand::RequestUserAttention(
            UserAttentionType::Critical
        )));
    }

    #[test]
    fn the_raised_window_goes_back_to_normal_afterwards() {
        let commands = ring_finished_commands();
        assert!(commands.contains(&ViewportCommand::WindowLevel(WindowLevel::Normal)));
        assert!(commands.contains(&ViewportCommand::RequestUserAttention(
            UserAttentionType::Reset
        )));
    }
}
