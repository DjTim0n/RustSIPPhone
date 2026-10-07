use crate::model::Account;
use crate::settings::TransportKind;
use base64::{Engine, engine::general_purpose::STANDARD};
use rsipstack::resolver::SipResolver;
use rsipstack::sip::{Domain, Port};
use rsipstack::transport::tls::TlsConfig;
use std::net::{IpAddr, SocketAddr, UdpSocket};

/// Finds the local IP the system uses to reach `target`.
/// Sends nothing: UDP `connect` only selects the route.
pub fn local_ip_towards(target: SocketAddr) -> std::io::Result<IpAddr> {
    let bind: SocketAddr = if target.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = UdpSocket::bind(bind)?;
    socket.connect(target)?;
    Ok(socket.local_addr()?.ip())
}

/// Every address the station can be reached at, best first.
///
/// `host` may be a name or an IP. Without a port, UDP, TCP and TLS look up the station's DNS SRV
/// records (RFC 3263) and fall back to the transport's default port; WS and WSS always use their
/// port (80 / 443 by default). IPv4 addresses come first because audio is IPv4-only.
pub async fn resolve_targets(
    host: &str,
    port: Option<u16>,
    transport: TransportKind,
) -> std::io::Result<Vec<SocketAddr>> {
    let port = port.or_else(|| (!transport.uses_srv()).then(|| transport.default_port()));
    let found = SipResolver::default()
        .lookup(
            &Domain::from(host),
            port.map(Port::from),
            Some(transport.sip()),
            transport.is_secure(),
        )
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;
    let mut addrs: Vec<SocketAddr> = found.into_iter().map(|target| target.addr).collect();
    // A stable sort keeps the order the SRV records gave us within each address family.
    addrs.sort_by_key(|addr| addr.is_ipv6());
    addrs.dedup();
    if addrs.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("could not resolve {host}"),
        ));
    }
    Ok(addrs)
}

fn pem_encode(der: &[u8]) -> String {
    let base64 = STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in base64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).unwrap_or_default());
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}

/// The root certificates to trust for TLS: the operating system's, plus an optional extra file
/// (a private CA, for example). The SIP stack trusts nothing by default, so without this a TLS
/// connection to a station with an ordinary certificate would be refused.
/// On failure returns the path of the extra file that could not be read.
pub fn tls_roots_pem(extra_ca_path: &str) -> Result<Vec<u8>, String> {
    let mut pem = String::new();
    for cert in rustls_native_certs::load_native_certs().certs {
        // The stack stops at the first certificate it cannot parse, so leave such ones out.
        if rustls::RootCertStore::empty().add(cert.clone()).is_ok() {
            pem.push_str(&pem_encode(cert.as_ref()));
        }
    }
    let extra = extra_ca_path.trim();
    if !extra.is_empty() {
        let text = std::fs::read_to_string(extra).map_err(|_| extra.to_string())?;
        pem.push_str(&text);
        if !text.ends_with('\n') {
            pem.push('\n');
        }
    }
    Ok(pem.into_bytes())
}

/// TLS settings for an account. The name the certificate is checked against is the SIP domain if
/// one is set (stations that sit behind SRV records present a certificate for the domain), else
/// the station's own name; a bare IP address is checked as an IP.
pub fn tls_config(account: &Account) -> Result<TlsConfig, String> {
    let (host, _) = account.server_host_port();
    let domain = account.connection.domain.trim();
    let sni_hostname = if !domain.is_empty() {
        Some(domain.to_string())
    } else if host.parse::<IpAddr>().is_err() {
        Some(host)
    } else {
        None
    };
    Ok(TlsConfig {
        ca_certs: Some(tls_roots_pem(&account.connection.ca_path)?),
        sni_hostname,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hostnames_resolve_with_the_default_port() {
        let addrs = resolve_targets("localhost", None, TransportKind::Udp)
            .await
            .expect("localhost resolves");
        assert!(
            addrs
                .iter()
                .any(|a| a.ip().is_loopback() && a.port() == 5060)
        );
    }

    #[tokio::test]
    async fn explicit_port_wins_and_ip_literals_need_no_dns() {
        let addrs = resolve_targets("192.0.2.7", Some(5080), TransportKind::Tcp)
            .await
            .unwrap();
        assert_eq!(addrs, vec!["192.0.2.7:5080".parse().unwrap()]);
    }

    #[tokio::test]
    async fn websocket_transports_default_to_their_ports() {
        let ws = resolve_targets("192.0.2.7", None, TransportKind::Ws)
            .await
            .unwrap();
        let wss = resolve_targets("192.0.2.7", None, TransportKind::Wss)
            .await
            .unwrap();
        assert_eq!(ws[0].port(), 80);
        assert_eq!(wss[0].port(), 443);
    }

    #[tokio::test]
    async fn tls_defaults_to_port_5061() {
        let addrs = resolve_targets("192.0.2.7", None, TransportKind::Tls)
            .await
            .unwrap();
        assert_eq!(addrs[0].port(), 5061);
    }

    #[tokio::test]
    async fn unknown_names_fail_cleanly() {
        let result = resolve_targets("no-such-host.invalid", None, TransportKind::Udp).await;
        assert!(result.is_err());
    }

    #[test]
    fn system_roots_form_valid_pem() {
        let pem = tls_roots_pem("").unwrap();
        // Every certificate we wrote must parse again; an empty store is also fine on bare systems.
        let text = String::from_utf8(pem).unwrap();
        assert_eq!(
            text.matches("BEGIN CERTIFICATE").count(),
            text.matches("END CERTIFICATE").count()
        );
    }

    #[test]
    fn missing_extra_certificate_file_is_reported_by_path() {
        assert_eq!(
            tls_roots_pem("/definitely/not/here.pem").unwrap_err(),
            "/definitely/not/here.pem"
        );
    }
}
