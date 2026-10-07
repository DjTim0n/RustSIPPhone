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
        let local_ip = net::local_ip_towards(server_addr)
            .map_err(|_| "Нет подключения к сети")?;
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
            events: events.clone(),
            slot,
        };

        let serve = tokio::spawn({
            let inner = endpoint.inner.clone();
            async move {
                let _ = inner.serve().await;
            }
        });
        let incoming = tokio::spawn(incoming_loop(endpoint.incoming_transactions().map_err(|e| e.to_string())?, ctx.clone()));
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
            Method::Invite => call::start_incoming(ctx.clone(), transaction),
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
