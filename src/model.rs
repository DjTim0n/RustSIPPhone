//! Shared types: what the phone core can do and what it reports to the interface.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// Station (PBX) address with the port, for example `192.168.1.10:5060`.
    pub server: String,
    /// Extension number (also the login).
    pub extension: String,
    pub password: String,
}

/// What the interface asks the core to do.
#[derive(Debug)]
pub enum Command {
    Register(Account),
    Unregister,
    Dial(String),
    Answer,
    Reject,
    Hangup,
    SetMute(bool),
    Dtmf(char),
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegState {
    Offline,
    Connecting,
    Online,
    /// `retry` means the core keeps trying on its own.
    Failed {
        notice: Notice,
        retry: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Incoming,
    Outgoing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Dialling, the station has not answered yet.
    Dialing,
    /// The other phone is ringing.
    Ringing,
    /// Someone is calling us, waiting for a decision.
    Incoming,
    /// The call is in progress.
    Active,
}

#[derive(Clone, Debug)]
pub struct CallView {
    pub peer: String,
    pub phase: Phase,
    pub connected_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Completed,
    Missed,
    Declined,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub number: String,
    pub direction: Direction,
    pub outcome: Outcome,
    /// Call start, seconds since 1970.
    pub started_at: i64,
    pub duration_secs: u64,
}

/// What the core reports to the interface.
#[derive(Debug)]
pub enum Event {
    Reg(RegState),
    Call(Option<CallView>),
    CallEnded {
        entry: HistoryEntry,
        /// What to show the user if the call ended without a conversation.
        notice: Option<Notice>,
    },
    Toast(Notice),
}

/// Sends events to the interface and wakes up its rendering.
#[derive(Clone)]
pub struct Events {
    tx: std::sync::mpsc::Sender<Event>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Events {
    pub fn new(tx: std::sync::mpsc::Sender<Event>, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Events { tx, repaint }
    }

    pub fn send(&self, event: Event) {
        let _ = self.tx.send(event);
        (self.repaint)();
    }
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A user-facing message, kept language-neutral so the interface can show it in any language
/// (and re-translate it when the language is switched). See `i18n.rs` for the texts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    SignInFirst,
    FinishCurrentCall,
    ServerNotFound,
    NoNetwork,
    PhoneStartFailed(String),
    ServerAddressInvalid,
    RegistrationRetrying,
    /// SIP status code the station answered the registration with.
    RegistrationRejected(u16),
    /// SIP status code the station answered the call with.
    CallRejected(u16),
    AudioUnavailable(String),
    SoundSetupFailed(String),
    CannotDial,
    ServerNotResponding,
    NoAnswerFromServer,
    SoundNegotiationFailed(String),
    IncomingNoCommonAudio(String),
    IncomingFailed,
    Missed(String),
    AnswerFailed,
    PeerEndedCall,
    NoAudioReceived,
    PasswordNotSaved,
}

/// Stored in place of a caller's number when the request does not say who is calling.
/// The interface shows it as a localized "Unknown number".
pub const UNKNOWN_PEER: &str = "?";
