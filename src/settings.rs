//! User-editable settings and the small helpers that interpret them.

use serde::{Deserialize, Serialize};

/// How the phone talks to the PBX.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportKind {
    #[default]
    Udp,
    Tcp,
    /// SIP over TLS: encrypted signalling.
    Tls,
    /// SIP over WebSocket (what browsers and some cloud PBXs use).
    Ws,
    /// SIP over secure WebSocket.
    Wss,
}

impl TransportKind {
    pub const ALL: [TransportKind; 5] = [
        TransportKind::Udp,
        TransportKind::Tcp,
        TransportKind::Tls,
        TransportKind::Ws,
        TransportKind::Wss,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TransportKind::Udp => "UDP",
            TransportKind::Tcp => "TCP",
            TransportKind::Tls => "TLS",
            TransportKind::Ws => "WS",
            TransportKind::Wss => "WSS",
        }
    }

    pub fn sip(self) -> rsipstack::sip::Transport {
        match self {
            TransportKind::Udp => rsipstack::sip::Transport::Udp,
            TransportKind::Tcp => rsipstack::sip::Transport::Tcp,
            TransportKind::Tls => rsipstack::sip::Transport::Tls,
            TransportKind::Ws => rsipstack::sip::Transport::Ws,
            TransportKind::Wss => rsipstack::sip::Transport::Wss,
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            TransportKind::Udp | TransportKind::Tcp => 5060,
            TransportKind::Tls => 5061,
            TransportKind::Ws => 80,
            TransportKind::Wss => 443,
        }
    }

    pub fn is_secure(self) -> bool {
        matches!(self, TransportKind::Tls | TransportKind::Wss)
    }

    /// Whether the station can be found through DNS SRV records when no port is given.
    pub fn uses_srv(self) -> bool {
        matches!(
            self,
            TransportKind::Udp | TransportKind::Tcp | TransportKind::Tls
        )
    }

    /// Everything except UDP keeps one long-lived connection to the station.
    pub fn is_stream(self) -> bool {
        self != TransportKind::Udp
    }
}

/// Connection options of an account. Everything here is optional with a sensible default, so an
/// account only needs the station address, extension and password to work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionSettings {
    /// Name shown to the other side as the caller's name.
    pub display_name: String,
    /// SIP domain when it differs from the station address.
    pub domain: String,
    pub transport: TransportKind,
    /// All traffic goes through this server instead of the station address (`host[:port]`).
    pub outbound_proxy: String,
    /// How long a registration lives before it has to be renewed, in seconds.
    pub expiry_secs: u32,
    /// Extra certificate authority (PEM file) to trust for TLS and WSS.
    pub ca_path: String,
    /// URL path of the WebSocket endpoint.
    pub ws_path: String,
    /// Advertise the public address the station sees (helps behind NAT).
    pub use_public_address: bool,
}

pub const MIN_EXPIRY_SECS: u32 = 30;
pub const MAX_EXPIRY_SECS: u32 = 3600;
pub const DEFAULT_EXPIRY_SECS: u32 = 60;

impl Default for ConnectionSettings {
    fn default() -> Self {
        ConnectionSettings {
            display_name: String::new(),
            domain: String::new(),
            transport: TransportKind::Udp,
            outbound_proxy: String::new(),
            expiry_secs: DEFAULT_EXPIRY_SECS,
            ca_path: String::new(),
            ws_path: "/".to_string(),
            use_public_address: true,
        }
    }
}

impl ConnectionSettings {
    /// The expiry clamped to what stations accept in practice.
    pub fn expiry(&self) -> u32 {
        self.expiry_secs.clamp(MIN_EXPIRY_SECS, MAX_EXPIRY_SECS)
    }
}

/// Which microphone and speaker to use. `None` means the system default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
}

/// Splits `host`, `host:port`, `[v6]` or `[v6]:port`. A bare IPv6 address (several colons, no
/// brackets) is taken as a host without a port.
pub fn split_host_port(text: &str) -> (String, Option<u16>) {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('[')
        && let Some((host, tail)) = rest.split_once(']')
    {
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
        return (host.to_string(), port);
    }
    if text.matches(':').count() == 1
        && let Some((host, port)) = text.split_once(':')
    {
        return (host.to_string(), port.parse().ok());
    }
    (text.to_string(), None)
}

/// Formats a host (and optional port) for use inside a SIP URI.
pub fn host_for_uri(host: &str, port: Option<u16>) -> String {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// Percent-encodes the characters that may not appear in the user part of a SIP URI.
pub fn escape_user(user: &str) -> String {
    user.chars()
        .map(|c| match c {
            '#' => "%23".to_string(),
            ' ' | '"' | '<' | '>' | '%' | '\\' => format!("%{:02X}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_hosts_and_ports() {
        assert_eq!(
            split_host_port("pbx.example.com"),
            ("pbx.example.com".into(), None)
        );
        assert_eq!(
            split_host_port("pbx.example.com:5080"),
            ("pbx.example.com".into(), Some(5080))
        );
        assert_eq!(
            split_host_port(" 10.0.0.1:5060 "),
            ("10.0.0.1".into(), Some(5060))
        );
        assert_eq!(
            split_host_port("[2001:db8::1]:5061"),
            ("2001:db8::1".into(), Some(5061))
        );
        assert_eq!(
            split_host_port("[2001:db8::1]"),
            ("2001:db8::1".into(), None)
        );
        assert_eq!(split_host_port("2001:db8::1"), ("2001:db8::1".into(), None));
    }

    #[test]
    fn formats_hosts_for_uris() {
        assert_eq!(host_for_uri("pbx.example.com", None), "pbx.example.com");
        assert_eq!(host_for_uri("10.0.0.1", Some(5060)), "10.0.0.1:5060");
        assert_eq!(
            host_for_uri("2001:db8::1", Some(5061)),
            "[2001:db8::1]:5061"
        );
    }

    #[test]
    fn escapes_hash_in_the_user_part() {
        assert_eq!(escape_user("*43#"), "*43%23");
        assert_eq!(escape_user("+7 777"), "+7%20777");
    }

    #[test]
    fn transports_have_sensible_defaults() {
        assert_eq!(TransportKind::default(), TransportKind::Udp);
        assert_eq!(TransportKind::Tls.default_port(), 5061);
        assert_eq!(TransportKind::Wss.default_port(), 443);
        assert!(TransportKind::Tls.is_secure() && TransportKind::Wss.is_secure());
        assert!(!TransportKind::Tcp.is_secure());
        assert!(TransportKind::Udp.uses_srv() && !TransportKind::Ws.uses_srv());
    }

    #[test]
    fn expiry_is_clamped() {
        let mut s = ConnectionSettings::default();
        s.expiry_secs = 5;
        assert_eq!(s.expiry(), MIN_EXPIRY_SECS);
        s.expiry_secs = 99_999;
        assert_eq!(s.expiry(), MAX_EXPIRY_SECS);
    }

    #[test]
    fn missing_settings_fields_use_defaults() {
        let s: ConnectionSettings = serde_json::from_str(r#"{"transport":"Tcp"}"#).unwrap();
        assert_eq!(s.transport, TransportKind::Tcp);
        assert_eq!(s.expiry_secs, DEFAULT_EXPIRY_SECS);
        assert!(s.use_public_address);
        assert_eq!(s.ws_path, "/");
    }
}
