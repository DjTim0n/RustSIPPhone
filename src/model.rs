//! Shared types: what the phone core can do and what it reports to the interface.

use crate::settings::{
    AudioSettings, CallSettings, ConnectionSettings, escape_user, host_for_uri, split_host_port,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Account {
    /// Station (PBX) address as typed: a name or an IP, with an optional port.
    pub server: String,
    /// Extension number (also the login).
    pub extension: String,
    pub password: String,
    pub connection: ConnectionSettings,
}

/// `Debug` must never print the password: it would end up in logs and bug reports.
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("server", &self.server)
            .field("extension", &self.extension)
            .field("password", &"<hidden>")
            .field("connection", &self.connection)
            .finish()
    }
}

impl Account {
    pub fn server_host_port(&self) -> (String, Option<u16>) {
        split_host_port(&self.server)
    }

    /// The host that goes after the `@` in SIP addresses: the SIP domain if one is set (without a
    /// port), otherwise the station address exactly as typed.
    fn uri_host(&self) -> String {
        let domain = self.connection.domain.trim();
        if domain.is_empty() {
            let (host, port) = self.server_host_port();
            host_for_uri(&host, port)
        } else {
            domain.to_string()
        }
    }

    /// Our own address, for example `sip:300@pbx.example.com`.
    pub fn own_uri(&self) -> String {
        format!("sip:{}@{}", escape_user(&self.extension), self.uri_host())
    }

    /// Where to register. The transport is not part of the address: the transport layer sends
    /// everything to the outbound address, which carries it.
    pub fn register_uri(&self) -> String {
        format!("sip:{}", self.uri_host())
    }

    /// The address to call for what the user typed: a plain number goes to our own station, a
    /// full address (`name@host`, with or without `sip:`) is used as it is.
    pub fn callee_uri(&self, dialled: &str) -> String {
        let dialled = dialled.trim();
        let bare = dialled
            .strip_prefix("sips:")
            .or_else(|| dialled.strip_prefix("sip:"))
            .unwrap_or(dialled);
        if bare.contains('@') {
            format!("sip:{bare}")
        } else {
            format!("sip:{}@{}", escape_user(bare), self.uri_host())
        }
    }
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
    /// Use these microphone and speaker for the calls that follow.
    SetAudio(AudioSettings),
    /// How calls behave: do not disturb, auto-answer, keypad tones.
    SetCalls(CallSettings),
    /// Put the current call on hold (`true`) or take it off hold (`false`).
    Hold(bool),
    /// Transfer the current call to this number or address.
    Transfer(String),
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
    /// We put the call on hold.
    pub local_hold: bool,
    /// The other side put the call on hold.
    pub remote_hold: bool,
    /// A transfer has been requested and not yet confirmed.
    pub transferring: bool,
}

impl CallView {
    pub fn new(peer: &str, phase: Phase) -> Self {
        CallView {
            peer: peer.to_string(),
            phase,
            connected_at: None,
            local_hold: false,
            remote_hold: false,
            transferring: false,
        }
    }
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
    /// The station refused to hold or resume the call; the code is its answer.
    HoldFailed(u16),
    /// The station refused the transfer; the code is its answer.
    TransferFailed(u16),
    /// A transfer was requested but the station never confirmed it.
    TransferUnconfirmed,
    /// The call was handed over to this number or address.
    Transferred(String),
    /// The connection to the station could not be set up; the text is the technical reason.
    ConnectionFailed(String),
    /// The secure connection failed, usually because of the station's certificate.
    TlsProblem(String),
    /// The extra certificate file could not be read.
    CertificateFileUnreadable(String),
}

/// Stored in place of a caller's number when the request does not say who is calling.
/// The interface shows it as a localized "Unknown number".
pub const UNKNOWN_PEER: &str = "?";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::TransportKind;

    fn account(server: &str, domain: &str, transport: TransportKind) -> Account {
        Account {
            server: server.into(),
            extension: "300".into(),
            password: "x".into(),
            connection: ConnectionSettings {
                domain: domain.into(),
                transport,
                ..Default::default()
            },
        }
    }

    #[test]
    fn debug_output_never_contains_the_password() {
        let mut a = account("pbx.example.com", "", TransportKind::Udp);
        a.password = "hunter2-secret".into();
        let text = format!("{a:?} {:?}", Command::Register(a.clone()));
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("<hidden>"));
    }

    #[test]
    fn plain_account_uses_the_station_address() {
        let a = account("10.0.0.1:5060", "", TransportKind::Udp);
        assert_eq!(a.own_uri(), "sip:300@10.0.0.1:5060");
        assert_eq!(a.register_uri(), "sip:10.0.0.1:5060");
    }

    #[test]
    fn domain_replaces_the_station_in_uris() {
        let a = account("sbc1.example.com:5061", "example.com", TransportKind::Tls);
        assert_eq!(a.own_uri(), "sip:300@example.com");
        assert_eq!(a.register_uri(), "sip:example.com");
    }

    #[test]
    fn callee_uri_handles_numbers_and_full_addresses() {
        let a = account("pbx.example.com", "", TransportKind::Udp);
        assert_eq!(a.callee_uri("100"), "sip:100@pbx.example.com");
        assert_eq!(a.callee_uri("*43#"), "sip:*43%23@pbx.example.com");
        assert_eq!(a.callee_uri("bob@other.org"), "sip:bob@other.org");
        assert_eq!(a.callee_uri("sip:bob@other.org"), "sip:bob@other.org");
        assert_eq!(a.callee_uri(" 100 "), "sip:100@pbx.example.com");
    }

    #[test]
    fn ipv6_station_is_bracketed() {
        let a = account("[2001:db8::1]:5060", "", TransportKind::Udp);
        assert_eq!(a.own_uri(), "sip:300@[2001:db8::1]:5060");
    }
}
