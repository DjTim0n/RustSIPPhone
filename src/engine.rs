//! Phone core: keeps the SIP registration, takes commands from the interface and starts calls.

use crate::call::{self, CallContext, CallCtl, CallSlot, LinkInfo, SharedLink};
use crate::model::*;
use crate::net;
use crate::settings::{AudioSettings, CallSettings, TransportKind, split_host_port};
use rsipstack::dialog::authenticate::Credential;
use rsipstack::dialog::dialog_layer::DialogLayer;
use rsipstack::dialog::registration::Registration;
use rsipstack::sip::{Auth, Host, HostWithPort, Method, Param, Scheme, StatusCode, Uri};
use rsipstack::transaction::Endpoint;
use rsipstack::transport::SipAddr;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const RETRY_AFTER: Duration = Duration::from_secs(15);

/// When to renew a registration that lives `expiry` seconds: well before it runs out, and often
/// enough to keep a NAT mapping open.
fn refresh_interval(expiry: u32) -> Duration {
    Duration::from_secs((u64::from(expiry) * 2 / 3).clamp(10, 3000))
}

pub async fn run(mut commands: UnboundedReceiver<Command>, events: Events) {
    let slot = Arc::new(CallSlot::default());
    let audio = Arc::new(Mutex::new(AudioSettings::default()));
    let calls = Arc::new(Mutex::new(CallSettings::default()));
    let mut session: Option<Session> = None;

    while let Some(command) = commands.recv().await {
        match command {
            Command::Register(account) => {
                if let Some(old) = session.take() {
                    old.stop().await;
                }
                events.send(Event::Reg(RegState::Connecting));
                match Session::start(
                    account,
                    events.clone(),
                    slot.clone(),
                    audio.clone(),
                    calls.clone(),
                )
                .await
                {
                    Ok(new) => session = Some(new),
                    Err(notice) => events.send(Event::Reg(RegState::Failed {
                        notice,
                        retry: false,
                    })),
                }
            }
            Command::Unregister => {
                slot.send(CallCtl::Hangup);
                if let Some(old) = session.take() {
                    old.stop().await;
                }
                events.send(Event::Reg(RegState::Offline));
            }
            Command::Dial(number) => match &session {
                Some(session) => {
                    if let Err(message) = call::start_outgoing(session.call_context(), number) {
                        events.send(Event::Toast(message));
                    }
                }
                None => events.send(Event::Toast(Notice::SignInFirst)),
            },
            Command::SetAudio(settings) => {
                *audio.lock().unwrap_or_else(|e| e.into_inner()) = settings
            }
            Command::SetCalls(settings) => {
                *calls.lock().unwrap_or_else(|e| e.into_inner()) = settings
            }
            Command::Hold(on) => slot.send(CallCtl::Hold(on)),
            Command::Transfer(target) => slot.send(CallCtl::Transfer(target)),
            Command::Answer => slot.send(CallCtl::Answer),
            Command::Reject => slot.send(CallCtl::Reject),
            Command::Hangup => slot.send(CallCtl::Hangup),
            Command::SetMute(on) => slot.send(CallCtl::Mute(on)),
            Command::Dtmf(digit) => slot.send(CallCtl::Dtmf(digit)),
            Command::Shutdown => break,
        }
    }

    // Shutting down: end the call and unregister so the station does not think we are online.
    slot.send(CallCtl::Hangup);
    for _ in 0..30 {
        if !slot.is_busy() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if let Some(old) = session.take() {
        old.stop().await;
    }
}

struct Session {
    endpoint: Endpoint,
    ctx: CallContext,
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    registration: JoinHandle<()>,
}

impl Session {
    async fn start(
        account: Account,
        events: Events,
        slot: Arc<CallSlot>,
        audio: Arc<Mutex<AudioSettings>>,
        calls: Arc<Mutex<CallSettings>>,
    ) -> Result<Session, Notice> {
        let transport = account.connection.transport;
        let (host, port) = account.server_host_port();
        let mut server_ips: Vec<IpAddr> = Vec::new();
        let targets = net::resolve_targets(&host, port, transport)
            .await
            .map_err(|_| Notice::ServerNotFound)?;
        server_ips.extend(targets.iter().map(|a| a.ip()));
        let mut first = targets[0];
        let proxy = account.connection.outbound_proxy.trim();
        if !proxy.is_empty() {
            let (proxy_host, proxy_port) = split_host_port(proxy);
            let proxy_targets = net::resolve_targets(&proxy_host, proxy_port, transport)
                .await
                .map_err(|_| Notice::ServerNotFound)?;
            server_ips.extend(proxy_targets.iter().map(|a| a.ip()));
            first = proxy_targets[0];
        }
        server_ips.dedup();
        let local_ip = net::local_ip_towards(first).map_err(|_| Notice::NoNetwork)?;
        let account = Arc::new(account);

        let cancel = CancellationToken::new();
        let endpoint = create_endpoint(local_ip, cancel.clone(), &account).await?;
        let dialog_layer = Arc::new(DialogLayer::new(endpoint.inner.clone()));
        let link: SharedLink = Arc::new(Mutex::new(None));
        let ctx = CallContext {
            dialog_layer,
            account: account.clone(),
            local_ip,
            server_ips: Arc::new(server_ips),
            link: link.clone(),
            audio,
            calls,
            events: events.clone(),
            slot,
        };

        let serve = tokio::spawn({
            let inner = endpoint.inner.clone();
            async move {
                let _ = inner.serve().await;
            }
        });
        let incoming = tokio::spawn(incoming_loop(
            endpoint
                .incoming_transactions()
                .map_err(|e| Notice::PhoneStartFailed(e.to_string()))?,
            ctx.clone(),
        ));
        let registration = tokio::spawn(registration_loop(
            endpoint.inner.clone(),
            account,
            events,
            cancel.clone(),
            link,
        ));

        Ok(Session {
            endpoint,
            ctx,
            cancel,
            tasks: vec![serve, incoming],
            registration,
        })
    }

    fn call_context(&self) -> CallContext {
        self.ctx.clone()
    }

    async fn stop(self) {
        // First let the registration remove itself from the station, then shut the stack down.
        self.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(4), self.registration).await;
        self.endpoint.shutdown();
        for task in self.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
    }
}

/// The address every request is sent to when that is not simply the host of its URI: the
/// outbound proxy if one is set, the station when the SIP domain is a different name, and for
/// every connection-oriented transport (TCP, TLS, WS, WSS), because the URI of a call does not say
/// which transport to use.
fn outbound_addr(account: &Account) -> Option<SipAddr> {
    let connection = &account.connection;
    let transport = connection.transport;
    let proxy = connection.outbound_proxy.trim();
    let (host, port) = account.server_host_port();
    let domain = connection.domain.trim();
    let domain_differs = !domain.is_empty() && !domain.eq_ignore_ascii_case(&host);
    if proxy.is_empty() && !domain_differs && !transport.is_stream() {
        return None;
    }
    let (host, port) = if proxy.is_empty() {
        (host, port)
    } else {
        split_host_port(proxy)
    };
    Some(sip_addr(&host, port, transport))
}

/// A SIP address for `host`: a name stays a name (so SRV records are looked up when there is no
/// port), an IP gets the transport's default port.
fn sip_addr(host: &str, port: Option<u16>, transport: TransportKind) -> SipAddr {
    let addr = match host.parse::<IpAddr>() {
        Ok(ip) => HostWithPort {
            host: Host::IpAddr(ip),
            port: Some(port.unwrap_or(transport.default_port()).into()),
        },
        Err(_) => HostWithPort {
            host: Host::Domain(host.into()),
            port: port.map(Into::into),
        },
    };
    SipAddr {
        r#type: Some(transport.sip()),
        addr,
    }
}

async fn create_endpoint(
    local_ip: IpAddr,
    cancel: CancellationToken,
    account: &Arc<Account>,
) -> Result<Endpoint, Notice> {
    let start_failed = |e: rsipstack::Error| Notice::PhoneStartFailed(e.to_string());
    // A UDP socket is always bound: it carries UDP calls and gives every other transport a local
    // address to build requests from. Bind to a specific address so Via/Contact name the
    // interface the station can see.
    let local_address = std::net::SocketAddr::new(local_ip, 0);
    let mut transport_layer = rsipstack::transport::TransportLayer::new(cancel.clone());
    let udp = rsipstack::transport::udp::UdpConnection::create_connection(
        local_address,
        None,
        Some(cancel.clone()),
    )
    .await
    .map_err(start_failed)?;
    transport_layer.add_transport(udp.into());
    transport_layer.outbound = outbound_addr(account);

    match account.connection.transport {
        TransportKind::Tls => {
            // Loading the system's certificates reads files, so keep it off the async threads.
            let for_tls = account.clone();
            let config = tokio::task::spawn_blocking(move || net::tls_config(&for_tls))
                .await
                .map_err(|e| Notice::PhoneStartFailed(e.to_string()))?
                .map_err(Notice::CertificateFileUnreadable)?;
            transport_layer.set_tls_config(config);
        }
        TransportKind::Ws | TransportKind::Wss => {
            transport_layer.set_ws_path(account.connection.ws_path.clone());
        }
        TransportKind::Udp | TransportKind::Tcp => {}
    }

    let mut builder = rsipstack::EndpointBuilder::new();
    builder.with_transport_layer(transport_layer);
    builder.with_cancel_token(cancel);
    builder.with_user_agent("RustSIPPhone/1.0");
    builder.with_inspector(Box::new(SourceStamp));
    Ok(builder.build())
}

/// Our own address as the station will see it. For a connection-oriented transport this opens
/// (and keeps) the connection, because only then is its local port known.
async fn local_address(
    endpoint: &rsipstack::transaction::endpoint::EndpointInnerRef,
    target: &Uri,
    transport: TransportKind,
) -> rsipstack::Result<HostWithPort> {
    if transport.is_stream() {
        let target = SipAddr::try_from(target)?;
        let (connection, _) = endpoint.transport_layer.lookup(&target, None).await?;
        Ok(connection.get_addr().addr.clone())
    } else {
        endpoint
            .transport_layer
            .get_addrs()
            .into_iter()
            .next()
            .map(|addr| addr.addr)
            .ok_or_else(|| rsipstack::Error::Error("no local address".into()))
    }
}

/// How an address looks in a Contact header: with the transport spelled out when it is not UDP.
fn contact_uri(account: &Account, host: HostWithPort) -> Uri {
    let transport = account.connection.transport;
    Uri {
        scheme: Some(Scheme::Sip),
        auth: Some(Auth {
            user: account.extension.clone(),
            password: None,
        }),
        host_with_port: host,
        params: if transport.is_stream() {
            vec![Param::Transport(transport.sip())]
        } else {
            Vec::new()
        },
        headers: Vec::new(),
    }
}

fn typed_contact(account: &Account, host: HostWithPort) -> rsipstack::sip::typed::Contact {
    rsipstack::sip::typed::Contact {
        display_name: None,
        uri: contact_uri(account, host),
        params: Vec::new(),
    }
}

/// Which notice describes a connection error best.
fn describe_error(error: &rsipstack::Error, transport: TransportKind) -> Notice {
    let text = error.to_string();
    let lower = text.to_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        Notice::RegistrationRetrying
    } else if transport.is_secure()
        && [
            "certificate",
            "tls",
            "handshake",
            "invalid dns name",
            "unknown issuer",
        ]
        .iter()
        .any(|word| lower.contains(word))
    {
        Notice::TlsProblem(text)
    } else {
        Notice::ConnectionFailed(text)
    }
}

async fn registration_loop(
    endpoint: rsipstack::transaction::endpoint::EndpointInnerRef,
    account: Arc<Account>,
    events: Events,
    cancel: CancellationToken,
    link: SharedLink,
) {
    let Ok(target) = account.register_uri().parse::<Uri>() else {
        events.send(Event::Reg(RegState::Failed {
            notice: Notice::ServerAddressInvalid,
            retry: false,
        }));
        return;
    };
    let transport = account.connection.transport;
    let expiry = account.connection.expiry();
    let mut registration = Registration::new(
        endpoint.clone(),
        Some(Credential {
            username: account.extension.clone(),
            password: account.password.clone(),
            realm: None,
        }),
    );

    let mut ever_online = false;
    let mut local: Option<HostWithPort> = None;
    loop {
        // Learn our local address first: the Contact header has to carry it (and, for the
        // connection-oriented transports, the transport), and calls reuse it.
        if local.is_none() {
            match local_address(&endpoint, &target, transport).await {
                Ok(address) => {
                    registration.contact = Some(typed_contact(&account, address.clone()));
                    local = Some(address);
                }
                Err(error) => {
                    let notice = describe_error(&error, transport);
                    // A certificate or configuration problem is not cured by waiting.
                    let retry = ever_online || !matches!(notice, Notice::TlsProblem(_));
                    events.send(Event::Reg(RegState::Failed { notice, retry }));
                    if !retry {
                        return;
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(RETRY_AFTER) => {}
                    }
                    continue;
                }
            }
        }

        let result = tokio::select! {
            _ = cancel.cancelled() => break,
            result = registration.register(target.clone(), Some(expiry)) => result,
        };
        let delay = match result {
            Ok(response) if response.status_code().code() == 200 => {
                ever_online = true;
                // Remember how the station sees us, for the Contact of calls and for the media
                // address, then keep using it for the next renewal too.
                let own = local.clone().expect("set above");
                let public = if account.connection.use_public_address {
                    registration.public_address.clone()
                } else {
                    None
                };
                let advertised = public.clone().unwrap_or(own);
                registration.contact = Some(typed_contact(&account, advertised.clone()));
                *link.lock().unwrap_or_else(|e| e.into_inner()) = Some(LinkInfo {
                    contact: contact_uri(&account, advertised),
                    public_ip: public.and_then(|address| match address.host {
                        Host::IpAddr(ip) => Some(ip),
                        Host::Domain(_) => None,
                    }),
                });
                events.send(Event::Reg(RegState::Online));
                refresh_interval(expiry)
            }
            Ok(response) => {
                let code = response.status_code().code();
                let credentials_rejected = matches!(code, 401 | 403 | 404 | 407);
                // A wrong password is not fixed by retrying: wait until the user corrects the details.
                let retry = ever_online || !credentials_rejected;
                events.send(Event::Reg(RegState::Failed {
                    notice: Notice::RegistrationRejected(code),
                    retry,
                }));
                if !retry {
                    return;
                }
                RETRY_AFTER
            }
            Err(error) => {
                // The connection may have been lost: learn our address again next time.
                local = None;
                events.send(Event::Reg(RegState::Failed {
                    notice: describe_error(&error, transport),
                    retry: true,
                }));
                RETRY_AFTER
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(delay) => {}
        }
    }

    if ever_online {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            registration.register(target, Some(0)),
        )
        .await;
    }
}

async fn incoming_loop(
    mut incoming: rsipstack::transaction::TransactionReceiver,
    ctx: CallContext,
) {
    while let Some(mut transaction) = incoming.recv().await {
        // Requests inside a dialog (BYE from the other side etc.) go to the dialog itself.
        if let Some(mut dialog) = ctx.dialog_layer.match_dialog(&transaction) {
            let _ = dialog.handle(&mut transaction).await;
            continue;
        }
        match transaction.original.method() {
            Method::Invite => {
                // UDP source addresses can be forged, but a stranger should not be able to make
                // the phone ring with an arbitrary caller name: only the server we registered
                // with may send us calls.
                let from_station = request_source_ip(&transaction.original)
                    .is_some_and(|ip| ctx.server_ips.contains(&ip));
                // The phone also keeps a plain UDP socket open. A call that did not come over the
                // transport the account uses (say, UDP when the account uses TLS) is refused, so
                // a forged packet cannot get around the encrypted channel.
                let right_transport = request_transport(&transaction.original)
                    == Some(ctx.account.connection.transport);
                if from_station && right_transport {
                    call::start_incoming(ctx.clone(), transaction);
                } else {
                    let _ = transaction.reply(StatusCode::Forbidden).await;
                }
            }
            Method::Options => {
                let _ = transaction.reply(StatusCode::OK).await;
            }
            Method::Ack => {}
            Method::Bye | Method::Cancel | Method::Update | Method::Info => {
                let _ = transaction
                    .reply(StatusCode::CallTransactionDoesNotExist)
                    .await;
            }
            _ => {
                let _ = transaction.reply(StatusCode::NotImplemented).await;
            }
        }
    }
}

/// Header the transport layer stamps on every received request with the address the packet
/// really came from. Anything a sender puts under this name is discarded first.
const SOURCE_HEADER: &str = "X-Verified-Source";
/// The transport the request arrived over, stamped the same way.
const TRANSPORT_HEADER: &str = "X-Verified-Transport";

/// Message inspector that records the true UDP source of each incoming request.
///
/// The `Via` header cannot be used for this: its `received` parameter is only rewritten when it
/// disagrees with the sent-by address, so a forged value can survive, and values after the first
/// comma are parsed differently by different code. The `from` address handed to the inspector
/// comes from the socket itself.
struct SourceStamp;

impl rsipstack::transaction::endpoint::MessageInspector for SourceStamp {
    fn before_send(
        &self,
        msg: rsipstack::sip::SipMessage,
        _dest: Option<&rsipstack::transport::SipAddr>,
    ) -> rsipstack::sip::SipMessage {
        msg
    }

    fn after_received(
        &self,
        mut msg: rsipstack::sip::SipMessage,
        from: Option<&rsipstack::transport::SipAddr>,
    ) -> rsipstack::sip::SipMessage {
        if let rsipstack::sip::SipMessage::Request(request) = &mut msg {
            request.headers.retain(|header| !is_stamp_header(header));
            if let Some(from) = from {
                request.headers.push(rsipstack::sip::Header::Other(
                    SOURCE_HEADER.to_string(),
                    from.addr.host.to_string(),
                ));
                if let Some(transport) = from.r#type.and_then(transport_kind) {
                    request.headers.push(rsipstack::sip::Header::Other(
                        TRANSPORT_HEADER.to_string(),
                        transport.label().to_string(),
                    ));
                }
            }
        }
        msg
    }
}

fn is_stamp_header(header: &rsipstack::sip::Header) -> bool {
    matches!(header, rsipstack::sip::Header::Other(name, _)
        if name.eq_ignore_ascii_case(SOURCE_HEADER) || name.eq_ignore_ascii_case(TRANSPORT_HEADER))
}

fn transport_kind(transport: rsipstack::sip::Transport) -> Option<TransportKind> {
    use rsipstack::sip::Transport;
    match transport {
        Transport::Udp => Some(TransportKind::Udp),
        Transport::Tcp => Some(TransportKind::Tcp),
        Transport::Tls => Some(TransportKind::Tls),
        Transport::Ws => Some(TransportKind::Ws),
        Transport::Wss => Some(TransportKind::Wss),
        Transport::Sctp | Transport::TlsSctp => None,
    }
}

/// The transport a request really arrived over, or `None` if it was not stamped.
fn request_transport(request: &rsipstack::sip::Request) -> Option<TransportKind> {
    request.headers.iter().find_map(|header| match header {
        rsipstack::sip::Header::Other(name, value)
            if name.eq_ignore_ascii_case(TRANSPORT_HEADER) =>
        {
            TransportKind::ALL
                .into_iter()
                .find(|kind| kind.label().eq_ignore_ascii_case(value.trim()))
        }
        _ => None,
    })
}

/// The IP a request really came from, or `None` if it was not stamped (treated as untrusted).
fn request_source_ip(request: &rsipstack::sip::Request) -> Option<std::net::IpAddr> {
    request.headers.iter().find_map(|header| match header {
        rsipstack::sip::Header::Other(name, value) if name.eq_ignore_ascii_case(SOURCE_HEADER) => {
            value.trim().parse().ok()
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const FORGED_INVITE: &str = "INVITE sip:300@127.0.0.1 SIP/2.0\r\n\
        Via: SIP/2.0/UDP 127.0.0.1:5999;branch=z9hG4bK1;received=203.0.113.9\r\n\
        Via: SIP/2.0/UDP 10.0.0.1;received=203.0.113.9\r\n\
        X-Verified-Source: 203.0.113.9\r\n\
        X-Verified-Transport: TLS\r\n\
        x-verified-source: 203.0.113.9\r\n\
        From: <sip:100@x>;tag=1\r\nTo: <sip:300@x>\r\nCall-ID: forged-1\r\n\
        CSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";

    /// A stranger on 127.0.0.1 forges every field that could name the server (203.0.113.9).
    /// The endpoint must still report the real source.
    #[tokio::test]
    async fn forged_headers_do_not_change_the_verified_source() {
        let cancel = CancellationToken::new();
        let endpoint = create_endpoint(
            "127.0.0.1".parse().unwrap(),
            cancel.clone(),
            &Arc::new(Account::default()),
        )
        .await
        .expect("endpoint");
        let target = endpoint.get_addrs()[0].addr.to_string();
        let mut incoming = endpoint.incoming_transactions().expect("incoming");
        let inner = endpoint.inner.clone();
        let serve = tokio::spawn(async move { inner.serve().await });

        let stranger = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        stranger
            .send_to(FORGED_INVITE.as_bytes(), &target)
            .await
            .unwrap();

        let transaction = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
            .await
            .expect("INVITE should reach the endpoint")
            .expect("channel open");
        assert_eq!(
            request_source_ip(&transaction.original),
            Some("127.0.0.1".parse().unwrap())
        );
        let stamps = transaction
            .original
            .headers
            .iter()
            .filter(|h| is_stamp_header(h))
            .count();
        assert_eq!(
            stamps, 2,
            "forged copies must be removed: one source and one transport stamp"
        );
        // The packet came over UDP, whatever the sender claims.
        assert_eq!(
            request_transport(&transaction.original),
            Some(TransportKind::Udp)
        );

        endpoint.shutdown();
        let _ = serve.await;
    }

    #[test]
    fn unstamped_request_is_untrusted() {
        let raw = "INVITE sip:300@x SIP/2.0\r\nVia: SIP/2.0/UDP 203.0.113.9:5060;branch=z9hG4bK1\r\n\
                   From: <sip:1@x>;tag=1\r\nTo: <sip:300@x>\r\nCall-ID: a\r\nCSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";
        let request = rsipstack::sip::Request::try_from(raw).expect("valid request");
        assert_eq!(request_source_ip(&request), None);
    }
}

/// End-to-end tests: the real phone core registers on a small station built from the same SIP
/// stack, over every transport.
#[cfg(test)]
mod registration_tests {
    use super::*;
    use crate::settings::ConnectionSettings;
    use rsipstack::sip::prelude::HeadersExt;
    use rsipstack::transport::udp::UdpConnection;
    use rsipstack::transport::{
        TcpListenerConnection, TlsConfig, TlsListenerConnection, TransportLayer,
        WebSocketListenerConnection,
    };

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A station that answers every REGISTER with 200 OK and remembers the Contact it was given.
    struct Station {
        port: u16,
        contacts: Arc<Mutex<Vec<String>>>,
        cancel: CancellationToken,
    }

    impl Drop for Station {
        fn drop(&mut self) {
            self.cancel.cancel();
        }
    }

    async fn start_station(transport: TransportKind, tls: Option<TlsConfig>) -> Station {
        let cancel = CancellationToken::new();
        let port = free_port();
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let layer = TransportLayer::new(cancel.clone());
        match transport {
            TransportKind::Udp => layer.add_transport(
                UdpConnection::create_connection(addr, None, Some(cancel.clone()))
                    .await
                    .unwrap()
                    .into(),
            ),
            TransportKind::Tcp => {
                layer.add_transport(TcpListenerConnection::new(addr, None).await.unwrap().into())
            }
            TransportKind::Tls => layer.add_transport(
                TlsListenerConnection::new(addr, None, tls.expect("TLS needs a certificate"))
                    .await
                    .unwrap()
                    .into(),
            ),
            TransportKind::Ws => layer.add_transport(
                WebSocketListenerConnection::new(addr, None, false)
                    .await
                    .unwrap()
                    .into(),
            ),
            TransportKind::Wss => panic!("not tested"),
        }
        let mut builder = rsipstack::EndpointBuilder::new();
        builder.with_transport_layer(layer);
        builder.with_cancel_token(cancel.clone());
        let endpoint = builder.build();
        let inner = endpoint.inner.clone();
        tokio::spawn(async move {
            let _ = inner.serve().await;
        });
        let mut incoming = endpoint.incoming_transactions().unwrap();
        let contacts = Arc::new(Mutex::new(Vec::new()));
        let seen = contacts.clone();
        tokio::spawn(async move {
            let _keep_alive = &endpoint;
            while let Some(mut transaction) = incoming.recv().await {
                if let Ok(contact) = transaction.original.contact_header() {
                    seen.lock().unwrap().push(contact.to_string());
                }
                let _ = transaction.reply(StatusCode::OK).await;
            }
        });
        // Let the listener come up before the phone dials.
        tokio::time::sleep(Duration::from_millis(300)).await;
        Station {
            port,
            contacts,
            cancel,
        }
    }

    fn account(port: u16, transport: TransportKind, ca_path: &str) -> Account {
        Account {
            server: format!("127.0.0.1:{port}"),
            extension: "300".into(),
            password: "secret".into(),
            connection: ConnectionSettings {
                transport,
                ca_path: ca_path.into(),
                ..Default::default()
            },
        }
    }

    /// Starts the phone core with `account` and returns the first verdict on its registration.
    async fn first_registration_state(account: Account) -> RegState {
        let (tx, rx) = std::sync::mpsc::channel();
        let events = Events::new(tx, Arc::new(|| {}));
        let session = Session::start(
            account,
            events,
            Arc::new(CallSlot::default()),
            Arc::new(Mutex::new(AudioSettings::default())),
            Arc::new(Mutex::new(CallSettings::default())),
        )
        .await
        .expect("the session starts");
        let state = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match rx.try_recv() {
                    Ok(Event::Reg(state @ (RegState::Online | RegState::Failed { .. }))) => {
                        return state;
                    }
                    _ => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        })
        .await
        .expect("a registration verdict within 10 seconds");
        session.stop().await;
        state
    }

    #[tokio::test]
    async fn registers_over_udp() {
        let station = start_station(TransportKind::Udp, None).await;
        let state = first_registration_state(account(station.port, TransportKind::Udp, "")).await;
        assert_eq!(state, RegState::Online);
        let contacts = station.contacts.lock().unwrap().clone();
        assert!(!contacts.is_empty());
        assert!(
            contacts
                .iter()
                .all(|c| !c.to_lowercase().contains("transport=")),
            "UDP contacts carry no transport: {contacts:?}"
        );
    }

    #[tokio::test]
    async fn registers_over_tcp_and_says_so_in_the_contact() {
        let station = start_station(TransportKind::Tcp, None).await;
        let state = first_registration_state(account(station.port, TransportKind::Tcp, "")).await;
        assert_eq!(state, RegState::Online);
        let contacts = station.contacts.lock().unwrap().clone();
        assert!(
            contacts
                .iter()
                .any(|c| c.to_lowercase().contains("transport=tcp")),
            "the contact must name TCP so the station calls back over it: {contacts:?}"
        );
    }

    #[tokio::test]
    async fn registers_over_websocket() {
        let station = start_station(TransportKind::Ws, None).await;
        let state = first_registration_state(account(station.port, TransportKind::Ws, "")).await;
        assert_eq!(state, RegState::Online);
        let contacts = station.contacts.lock().unwrap().clone();
        assert!(
            contacts
                .iter()
                .any(|c| c.to_lowercase().contains("transport=ws")),
            "{contacts:?}"
        );
    }

    fn self_signed() -> (TlsConfig, String) {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let pem = cert.pem();
        let config = TlsConfig {
            cert: Some(pem.clone().into_bytes()),
            key: Some(signing_key.serialize_pem().into_bytes()),
            ..Default::default()
        };
        (config, pem)
    }

    #[tokio::test]
    async fn registers_over_tls_when_the_certificate_is_trusted() {
        let (config, pem) = self_signed();
        let station = start_station(TransportKind::Tls, Some(config)).await;
        let ca_file = std::env::temp_dir().join(format!("rsp-test-ca-{}.pem", station.port));
        std::fs::write(&ca_file, pem).unwrap();
        let state = first_registration_state(account(
            station.port,
            TransportKind::Tls,
            ca_file.to_str().unwrap(),
        ))
        .await;
        let _ = std::fs::remove_file(&ca_file);
        assert_eq!(state, RegState::Online);
        let contacts = station.contacts.lock().unwrap().clone();
        assert!(
            contacts
                .iter()
                .any(|c| c.to_lowercase().contains("transport=tls")),
            "{contacts:?}"
        );
    }

    #[tokio::test]
    async fn tls_with_an_unknown_certificate_is_refused_with_a_clear_notice() {
        let (config, _pem) = self_signed();
        let station = start_station(TransportKind::Tls, Some(config)).await;
        // No extra CA given: a self-signed station certificate must not be trusted.
        let state = first_registration_state(account(station.port, TransportKind::Tls, "")).await;
        assert!(
            matches!(
                state,
                RegState::Failed {
                    notice: Notice::TlsProblem(_),
                    retry: false
                }
            ),
            "{state:?}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_station_is_reported_and_retried() {
        // Nothing listens on this port.
        let state = first_registration_state(account(free_port(), TransportKind::Tcp, "")).await;
        assert!(
            matches!(
                state,
                RegState::Failed {
                    notice: Notice::ConnectionFailed(_),
                    retry: true
                }
            ),
            "{state:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_certificate_file_is_reported_by_path() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let events = Events::new(tx, Arc::new(|| {}));
        let result = Session::start(
            account(5061, TransportKind::Tls, "/definitely/not/here.pem"),
            events,
            Arc::new(CallSlot::default()),
            Arc::new(Mutex::new(AudioSettings::default())),
            Arc::new(Mutex::new(CallSettings::default())),
        )
        .await;
        assert!(matches!(
            result,
            Err(Notice::CertificateFileUnreadable(path)) if path == "/definitely/not/here.pem"
        ));
    }

    #[test]
    fn outbound_address_depends_on_transport_domain_and_proxy() {
        let mut a = account(5060, TransportKind::Udp, "");
        assert!(outbound_addr(&a).is_none(), "plain UDP needs none");

        a.connection.transport = TransportKind::Tcp;
        let tcp = outbound_addr(&a).expect("a stream transport pins the address");
        assert_eq!(tcp.r#type, Some(rsipstack::sip::Transport::Tcp));

        a.connection.transport = TransportKind::Udp;
        a.connection.domain = "example.com".into();
        assert!(
            outbound_addr(&a).is_some(),
            "a different domain pins the station"
        );

        a.connection.domain.clear();
        a.connection.outbound_proxy = "proxy.example.com".into();
        let proxy = outbound_addr(&a).unwrap();
        assert_eq!(proxy.addr.to_string(), "proxy.example.com");

        // A name without a port is left open for SRV; an IP gets the default port.
        assert_eq!(
            sip_addr("pbx.example.com", None, TransportKind::Tls)
                .addr
                .port,
            None
        );
        assert_eq!(
            sip_addr("10.0.0.1", None, TransportKind::Tls).addr.port,
            Some(5061.into())
        );
    }
}
