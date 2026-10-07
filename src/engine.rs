//! Ядро телефона: держит SIP-регистрацию, принимает команды интерфейса и запускает звонки.

use crate::call::{self, CallContext, CallCtl, CallSlot};
use crate::model::*;
use crate::net;
use rsipstack::dialog::authenticate::Credential;
use rsipstack::dialog::dialog_layer::DialogLayer;
use rsipstack::dialog::registration::Registration;
use rsipstack::sip::{Method, StatusCode};
use rsipstack::transaction::Endpoint;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Как часто обновляем регистрацию (срок жизни — 60 с).
const REFRESH_EVERY: Duration = Duration::from_secs(40);
const RETRY_AFTER: Duration = Duration::from_secs(15);

pub async fn run(mut commands: UnboundedReceiver<Command>, events: Events) {
    let slot = Arc::new(CallSlot::default());
    let mut session: Option<Session> = None;

    while let Some(command) = commands.recv().await {
        match command {
            Command::Register(account) => {
                if let Some(old) = session.take() {
                    old.stop().await;
                }
                events.send(Event::Reg(RegState::Connecting));
                match Session::start(account, events.clone(), slot.clone()).await {
                    Ok(new) => session = Some(new),
                    Err(message) => events.send(Event::Reg(RegState::Failed {
                        message,
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
                None => events.send(Event::Toast("Сначала войдите в аккаунт".into())),
            },
            Command::Answer => slot.send(CallCtl::Answer),
            Command::Reject => slot.send(CallCtl::Reject),
            Command::Hangup => slot.send(CallCtl::Hangup),
            Command::SetMute(on) => slot.send(CallCtl::Mute(on)),
            Command::Dtmf(digit) => slot.send(CallCtl::Dtmf(digit)),
            Command::Shutdown => break,
        }
    }

    // Выходим: завершаем звонок и снимаем регистрацию, чтобы станция не считала нас онлайн.
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
    ) -> Result<Session, String> {
        let server_addr = net::resolve(&account.server)
            .await
            .map_err(|_| "Не удалось найти станцию по этому адресу. Проверьте, как он написан")?;
        let local_ip = net::local_ip_towards(server_addr).map_err(|_| "Нет подключения к сети")?;
        let account = Arc::new(account);

        let cancel = CancellationToken::new();
        let endpoint = create_endpoint(local_ip, cancel.clone())
            .await
            .map_err(|e| format!("Не удалось запустить телефон: {e}"))?;
        let dialog_layer = Arc::new(DialogLayer::new(endpoint.inner.clone()));
        let ctx = CallContext {
            dialog_layer,
            account: account.clone(),
            local_ip,
            server_ip: server_addr.ip(),
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
                .map_err(|e| e.to_string())?,
            ctx.clone(),
        ));
        let registration = tokio::spawn(registration_loop(
            endpoint.inner.clone(),
            account,
            events,
            cancel.clone(),
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
        // Сначала даём регистрации снять себя со станции, потом гасим сам стек.
        self.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(4), self.registration).await;
        self.endpoint.shutdown();
        for task in self.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
    }
}

async fn create_endpoint(
    local_ip: std::net::IpAddr,
    cancel: CancellationToken,
) -> rsipstack::Result<Endpoint> {
    // Привязываемся к конкретному адресу, чтобы Via/Contact указывали на тот интерфейс, что видит станция.
    let local_address = std::net::SocketAddr::new(local_ip, 0);
    let transport_layer = rsipstack::transport::TransportLayer::new(cancel.clone());
    let udp = rsipstack::transport::udp::UdpConnection::create_connection(
        local_address,
        None,
        Some(cancel.clone()),
    )
    .await?;
    transport_layer.add_transport(udp.into());
    let mut builder = rsipstack::EndpointBuilder::new();
    builder.with_transport_layer(transport_layer);
    builder.with_cancel_token(cancel);
    builder.with_user_agent("RustSIPPhone/0.1");
    builder.with_inspector(Box::new(SourceStamp));
    Ok(builder.build())
}

async fn registration_loop(
    endpoint: rsipstack::transaction::endpoint::EndpointInnerRef,
    account: Arc<Account>,
    events: Events,
    cancel: CancellationToken,
) {
    let Ok(target) = format!("sip:{}", account.server).parse::<rsipstack::sip::Uri>() else {
        events.send(Event::Reg(RegState::Failed {
            message: "Адрес станции записан неверно".into(),
            retry: false,
        }));
        return;
    };
    let mut registration = Registration::new(
        endpoint,
        Some(Credential {
            username: account.extension.clone(),
            password: account.password.clone(),
            realm: None,
        }),
    );

    let mut ever_online = false;
    loop {
        let result = tokio::select! {
            _ = cancel.cancelled() => break,
            result = registration.register(target.clone(), Some(60)) => result,
        };
        let delay = match result {
            Ok(response) if response.status_code().code() == 200 => {
                ever_online = true;
                events.send(Event::Reg(RegState::Online));
                REFRESH_EVERY
            }
            Ok(response) => {
                let code = response.status_code().code();
                let credentials_rejected = matches!(code, 401 | 403 | 404 | 407);
                // Неверный пароль не лечится повторами — ждём, пока человек поправит данные.
                let retry = ever_online || !credentials_rejected;
                events.send(Event::Reg(RegState::Failed {
                    message: describe_register_status(code),
                    retry,
                }));
                if !retry {
                    return;
                }
                RETRY_AFTER
            }
            Err(_) => {
                events.send(Event::Reg(RegState::Failed {
                    message: "Станция не отвечает. Пробуем снова".into(),
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
        // Запросы внутри диалога (BYE от собеседника и т.п.) отдаём самому диалогу.
        if let Some(mut dialog) = ctx.dialog_layer.match_dialog(&transaction) {
            let _ = dialog.handle(&mut transaction).await;
            continue;
        }
        match transaction.original.method() {
            Method::Invite => {
                // UDP source addresses can be forged, but a stranger should not be able to make
                // the phone ring with an arbitrary caller name: only the server we registered
                // with may send us calls.
                if request_source_ip(&transaction.original) == Some(ctx.server_ip) {
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
            request.headers.retain(|header| !is_source_header(header));
            if let Some(from) = from {
                request.headers.push(rsipstack::sip::Header::Other(
                    SOURCE_HEADER.to_string(),
                    from.addr.host.to_string(),
                ));
            }
        }
        msg
    }
}

fn is_source_header(header: &rsipstack::sip::Header) -> bool {
    matches!(header, rsipstack::sip::Header::Other(name, _) if name.eq_ignore_ascii_case(SOURCE_HEADER))
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
        x-verified-source: 203.0.113.9\r\n\
        From: <sip:100@x>;tag=1\r\nTo: <sip:300@x>\r\nCall-ID: forged-1\r\n\
        CSeq: 1 INVITE\r\nContent-Length: 0\r\n\r\n";

    /// A stranger on 127.0.0.1 forges every field that could name the server (203.0.113.9).
    /// The endpoint must still report the real source.
    #[tokio::test]
    async fn forged_headers_do_not_change_the_verified_source() {
        let cancel = CancellationToken::new();
        let endpoint = create_endpoint("127.0.0.1".parse().unwrap(), cancel.clone())
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
            .filter(|h| is_source_header(h))
            .count();
        assert_eq!(stamps, 1, "forged copies must be removed");

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
