//! Window behaviour: a fixed size, staying alive in the background and surfacing on incoming calls.
//!
//! A phone has to keep running (and stay registered) when its window is closed, otherwise it
//! cannot ring. So closing the window only minimizes it, and an incoming call brings it back.
//! Minimizing, rather than hiding, keeps the Dock icon / taskbar button as the way back in.

use eframe::egui::{UserAttentionType, ViewportCommand, WindowLevel};

/// The window cannot be resized, so this is its exact inner size.
pub const WINDOW_SIZE: [f32; 2] = [400.0, 720.0];

/// What to do when the user asks to close the window.
///
/// Normally the request is cancelled and the window is minimized instead, so the phone keeps
/// running. When the user chose to quit, the request goes through and the app exits.
pub fn close_request_commands(quitting: bool) -> Vec<ViewportCommand> {
    if quitting {
        Vec::new()
    } else {
        vec![
            ViewportCommand::CancelClose,
            ViewportCommand::Minimized(true),
        ]
    }
}

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
    fn closing_the_window_minimizes_instead_of_quitting() {
        let commands = close_request_commands(false);
        assert!(commands.contains(&ViewportCommand::CancelClose));
        assert!(commands.contains(&ViewportCommand::Minimized(true)));
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
