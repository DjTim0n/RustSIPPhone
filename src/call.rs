//! Звонки: исходящий (INVITE) и входящий, разговор с RTP-медиа, завершение.

use crate::audio::AudioIo;
use crate::media::{self, MediaControl};
use crate::model::*;
use crate::ringtone::Ringtone;
use crate::sdp;
use rsipstack::dialog::authenticate::Credential;
use rsipstack::dialog::dialog::DialogState;
use rsipstack::dialog::dialog_layer::DialogLayer;
use rsipstack::dialog::invitation::InviteOption;
use rsipstack::dialog::invite_dialog::InviteDialog;
use rsipstack::sip::StatusCodeKind;
use rsipstack::sip::prelude::{HeadersExt, ToTypedHeader};
use rsipstack::transaction::transaction::Transaction;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

/// Сколько телефон звонит, пока никто не ответил.
const INCOMING_RING_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug)]
pub enum CallCtl {
    Answer,
    Reject,
    Hangup,
    Mute(bool),
    Dtmf(char),
}

/// Одновременно у нас не больше одного звонка.
#[derive(Default)]
pub struct CallSlot(Mutex<Option<UnboundedSender<CallCtl>>>);

impl CallSlot {
    fn claim(self: &Arc<Self>) -> Option<(UnboundedReceiver<CallCtl>, SlotGuard)> {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return None;
        }
        let (tx, rx) = unbounded_channel();
        *slot = Some(tx);
        Some((rx, SlotGuard(self.clone())))
    }

    pub fn is_busy(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn send(&self, ctl: CallCtl) {
        if let Some(tx) = self.0.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(ctl);
        }
    }
}

struct SlotGuard(Arc<CallSlot>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[derive(Clone)]
pub struct CallContext {
    pub dialog_layer: Arc<DialogLayer>,
    pub account: Arc<Account>,
    pub local_ip: IpAddr,
    /// Address of the SIP server; media from it is trusted.
    pub server_ip: IpAddr,
    pub events: Events,
    pub slot: Arc<CallSlot>,
}

struct Finished {
    outcome: Outcome,
    message: Option<String>,
    connected_at: Option<Instant>,
}

impl Finished {
    fn failed(message: impl Into<String>) -> Self {
        Finished {
            outcome: Outcome::Failed,
            message: Some(message.into()),
            connected_at: None,
        }
    }

    fn without_talk(outcome: Outcome, message: Option<String>) -> Self {
        Finished {
            outcome,
            message,
            connected_at: None,
        }
    }
}

fn publish(ctx: &CallContext, peer: &str, phase: Phase) {
    ctx.events.send(Event::Call(Some(CallView {
        peer: peer.to_string(),
        phase,
        connected_at: None,
    })));
}

fn finish(ctx: &CallContext, direction: Direction, peer: &str, started_at: i64, done: Finished) {
    let duration_secs = done
        .connected_at
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    ctx.events.send(Event::CallEnded {
        entry: HistoryEntry {
            number: peer.to_string(),
            direction,
            outcome: done.outcome,
            started_at,
            duration_secs,
        },
        message: done.message,
    });
    ctx.events.send(Event::Call(None));
}

// ---------------------------------------------------------------- исходящий

pub fn start_outgoing(ctx: CallContext, number: String) -> Result<(), String> {
    let Some((ctl_rx, guard)) = ctx.slot.claim() else {
        return Err("Сначала завершите текущий звонок".into());
    };
    tokio::spawn(async move {
        let _guard = guard;
        let started_at = now_unix();
        publish(&ctx, &number, Phase::Dialing);
        let done = outgoing(&ctx, &number, ctl_rx).await;
        finish(&ctx, Direction::Outgoing, &number, started_at, done);
    });
    Ok(())
}

async fn outgoing(
    ctx: &CallContext,
    number: &str,
    mut ctl_rx: UnboundedReceiver<CallCtl>,
) -> Finished {
    let account = &ctx.account;

    // Устройства открываем до INVITE: если нет доступа к микрофону, лучше узнать сразу.
    let audio = match AudioIo::start() {
        Ok(audio) => audio,
        Err(err) => return Finished::failed(audio_error_text(&err)),
    };
    let rtp_socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(socket) => Arc::new(socket),
        Err(err) => return Finished::failed(format!("Не удалось подготовить звук: {err}")),
    };
    let rtp_port = rtp_socket.local_addr().map(|a| a.port()).unwrap_or(0);
    let offer = sdp::build_offer(ctx.local_ip, rtp_port, now_unix() as u64);

    let parse_uri = |text: String| text.parse::<rsipstack::sip::Uri>().map_err(|e| e.to_string());
    let build_invite = || -> Result<InviteOption, String> {
        Ok(InviteOption {
            caller: parse_uri(format!("sip:{}@{}", account.extension, account.server))?,
            callee: parse_uri(format!("sip:{}@{}", number, account.server))?,
            contact: ctx
                .dialog_layer
                .build_local_contact(Some(account.extension.clone()), None)
                .map_err(|e| e.to_string())?,
            content_type: Some("application/sdp".to_string()),
            offer: Some(offer.clone().into_bytes()),
            credential: Some(credential(account)),
            ..Default::default()
        })
    };
    let invite = match build_invite() {
        Ok(invite) => invite,
        Err(_) => return Finished::failed("Не удалось набрать этот номер. Проверьте, что он введён верно"),
    };

    let (state_tx, mut state_rx) = ctx.dialog_layer.new_dialog_state_channel();
    let invite_future = ctx.dialog_layer.do_invite(invite, state_tx);
    tokio::pin!(invite_future);

    let (dialog, response) = loop {
        tokio::select! {
            result = &mut invite_future => match result {
                Ok(pair) => break pair,
                Err(_) => return Finished::failed("Станция не отвечает. Проверьте подключение к сети"),
            },
            Some(state) = state_rx.recv() => {
                if matches!(state, DialogState::Early(..)) {
                    publish(ctx, number, Phase::Ringing);
                }
            }
            ctl = ctl_rx.recv() => match ctl {
                // Если отбросить future, rsipstack сам отправит CANCEL.
                Some(CallCtl::Hangup | CallCtl::Reject) | None => {
                    return Finished::without_talk(Outcome::Cancelled, None);
                }
                _ => {}
            },
        }
    };

    let Some(response) = response else {
        return Finished::failed("Станция не ответила на звонок");
    };
    if response.status_code.kind() != StatusCodeKind::Successful {
        let code = response.status_code.code();
        return Finished::without_talk(Outcome::Failed, Some(describe_call_status(code)));
    }

    let result = match response_body(&response).and_then(|body| sdp::parse_remote(&body)) {
        Ok(remote) => {
            talk(
                ctx,
                &dialog,
                number,
                remote,
                rtp_socket,
                audio,
                &mut state_rx,
                &mut ctl_rx,
            )
            .await
        }
        Err(err) => {
            let _ = dialog.bye().await;
            Finished::failed(format!("Не удалось договориться о звуке: {err}"))
        }
    };
    ctx.dialog_layer.remove_dialog(&dialog.id());
    result
}

fn response_body(response: &rsipstack::sip::Response) -> Result<String, String> {
    String::from_utf8(response.body().to_vec()).map_err(|_| "ответ не в UTF-8".to_string())
}

// ----------------------------------------------------------------- входящий

pub fn start_incoming(ctx: CallContext, tx: Transaction) {
    let Some((ctl_rx, guard)) = ctx.slot.claim() else {
        // Уже разговариваем: звонящему — «занято».
        tokio::spawn(async move {
            let mut tx = tx;
            let _ = tx.reply(rsipstack::sip::StatusCode::BusyHere).await;
        });
        return;
    };
    tokio::spawn(async move {
        let _guard = guard;
        let started_at = now_unix();
        let peer = caller_of(&tx.original);
        let done = incoming(&ctx, tx, &peer, ctl_rx).await;
        finish(&ctx, Direction::Incoming, &peer, started_at, done);
    });
}

fn caller_of(request: &rsipstack::sip::Request) -> String {
    let Ok(from) = request.from_header().and_then(|h| h.typed()) else {
        return "Неизвестный номер".to_string();
    };
    let number = from
        .uri
        .auth
        .as_ref()
        .map(|auth| auth.user.clone())
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| from.uri.host_with_port.to_string());
    match from.display_name.as_deref().map(|n| n.trim_matches('"').trim()) {
        Some(name) if !name.is_empty() && name != number => format!("{name} ({number})"),
        _ => number,
    }
}

async fn incoming(
    ctx: &CallContext,
    mut tx: Transaction,
    peer: &str,
    mut ctl_rx: UnboundedReceiver<CallCtl>,
) -> Finished {
    use rsipstack::sip::StatusCode;

    let remote = match String::from_utf8(tx.original.body().to_vec())
        .map_err(|_| "в запросе нет звука".to_string())
        .and_then(|body| sdp::parse_remote(&body))
    {
        Ok(remote) => remote,
        Err(_) => {
            let _ = tx.reply(StatusCode::NotAcceptableHere).await;
            return Finished::failed(format!("Не удалось принять звонок от {peer}: нет общего способа передачи звука"));
        }
    };

    let (state_tx, mut state_rx) = ctx.dialog_layer.new_dialog_state_channel();
    let dialog = match ctx.dialog_layer.get_or_create_server_invite(
        &tx,
        state_tx,
        Some(credential(&ctx.account)),
        None,
    ) {
        Ok(dialog) => dialog,
        Err(_) => {
            let _ = tx.reply(StatusCode::ServerInternalError).await;
            return Finished::failed("Не удалось принять входящий звонок");
        }
    };
    // Обработчик ждёт ACK или CANCEL от звонящего и двигает состояние диалога.
    let mut handler_dialog = dialog.clone();
    tokio::spawn(async move {
        let _ = handler_dialog.handle(&mut tx).await;
    });
    let _ = dialog.ringing(None, None);

    publish(ctx, peer, Phase::Incoming);
    // Нет динамика или сигнал не запустился — звонок всё равно виден на экране.
    let ringtone = tokio::task::spawn_blocking(Ringtone::start)
        .await
        .ok()
        .and_then(Result::ok);

    let decision = tokio::time::timeout(INCOMING_RING_TIMEOUT, async {
        loop {
            tokio::select! {
                ctl = ctl_rx.recv() => match ctl {
                    Some(CallCtl::Answer) => return Decision::Answer,
                    Some(CallCtl::Reject | CallCtl::Hangup) => return Decision::Decline,
                    Some(_) => {}
                    None => return Decision::Decline,
                },
                Some(state) = state_rx.recv() => {
                    if matches!(state, DialogState::Terminated(..)) {
                        return Decision::CallerGaveUp;
                    }
                }
            }
        }
    })
    .await;
    drop(ringtone);

    match decision {
        Ok(Decision::Answer) => {}
        Ok(Decision::Decline) => {
            let _ = dialog.reject(Some(StatusCode::Decline), None);
            ctx.dialog_layer.remove_dialog(&dialog.id());
            return Finished::without_talk(Outcome::Declined, None);
        }
        Ok(Decision::CallerGaveUp) => {
            ctx.dialog_layer.remove_dialog(&dialog.id());
            return Finished::without_talk(
                Outcome::Missed,
                Some(format!("Пропущенный звонок: {peer}")),
            );
        }
        Err(_) => {
            let _ = dialog.reject(Some(StatusCode::TemporarilyUnavailable), None);
            ctx.dialog_layer.remove_dialog(&dialog.id());
            return Finished::without_talk(Outcome::Missed, Some(format!("Пропущенный звонок: {peer}")));
        }
    }

    let audio = match AudioIo::start() {
        Ok(audio) => audio,
        Err(err) => {
            let _ = dialog.reject(Some(StatusCode::ServerInternalError), None);
            ctx.dialog_layer.remove_dialog(&dialog.id());
            return Finished::failed(audio_error_text(&err));
        }
    };
    let rtp_socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(socket) => Arc::new(socket),
        Err(err) => {
            let _ = dialog.reject(Some(StatusCode::ServerInternalError), None);
            ctx.dialog_layer.remove_dialog(&dialog.id());
            return Finished::failed(format!("Не удалось подготовить звук: {err}"));
        }
    };
    let rtp_port = rtp_socket.local_addr().map(|a| a.port()).unwrap_or(0);
    let answer = sdp::build_sdp(
        ctx.local_ip,
        rtp_port,
        now_unix() as u64,
        &[remote.codec],
        remote.dtmf_pt,
    );
    let headers = vec![rsipstack::sip::Header::ContentType("application/sdp".into())];
    if dialog.accept(Some(headers), Some(answer.into_bytes())).is_err() {
        ctx.dialog_layer.remove_dialog(&dialog.id());
        return Finished::failed("Не удалось ответить на звонок");
    }

    let result = talk(
        ctx,
        &dialog,
        peer,
        remote,
        rtp_socket,
        audio,
        &mut state_rx,
        &mut ctl_rx,
    )
    .await;
    ctx.dialog_layer.remove_dialog(&dialog.id());
    result
}

enum Decision {
    Answer,
    Decline,
    CallerGaveUp,
}

// ----------------------------------------------------------------- разговор

#[allow(clippy::too_many_arguments)]
async fn talk(
    ctx: &CallContext,
    dialog: &InviteDialog,
    peer: &str,
    remote: sdp::Remote,
    rtp_socket: Arc<UdpSocket>,
    audio: AudioIo,
    state_rx: &mut UnboundedReceiver<DialogState>,
    ctl_rx: &mut UnboundedReceiver<CallCtl>,
) -> Finished {
    let connected_at = Instant::now();
    ctx.events.send(Event::Call(Some(CallView {
        peer: peer.to_string(),
        phase: Phase::Active,
        connected_at: Some(connected_at),
    })));

    let muted = Arc::new(AtomicBool::new(false));
    let (dtmf_tx, dtmf_rx) = unbounded_channel();
    let stop = CancellationToken::new();
    let media_task = tokio::spawn(media::run(
        rtp_socket,
        remote,
        audio,
        MediaControl {
            muted: muted.clone(),
            dtmf: dtmf_rx,
        },
        ctx.server_ip,
        stop.clone(),
    ));

    let message = loop {
        tokio::select! {
            ctl = ctl_rx.recv() => match ctl {
                Some(CallCtl::Hangup | CallCtl::Reject) | None => {
                    let _ = dialog.bye().await;
                    break None;
                }
                Some(CallCtl::Mute(on)) => muted.store(on, Ordering::Relaxed),
                Some(CallCtl::Dtmf(digit)) => { let _ = dtmf_tx.send(digit); }
                Some(CallCtl::Answer) => {}
            },
            state = state_rx.recv() => match state {
                Some(DialogState::Terminated(..)) | None => {
                    break Some("Собеседник завершил разговор".to_string());
                }
                Some(_) => {}
            },
        }
    };

    stop.cancel();
    let stats = media_task.await.unwrap_or_default();
    // Если за заметное время не пришло ни одного пакета звука, скорее всего его блокирует сеть.
    let silent = stats.received == 0 && connected_at.elapsed() > Duration::from_secs(5);
    let message = match (message, silent) {
        (_, true) => Some(
            "Собеседника не было слышно: звук от него не приходил. Возможно, мешают настройки сети".to_string(),
        ),
        (message, false) => message,
    };
    Finished {
        outcome: Outcome::Completed,
        message,
        connected_at: Some(connected_at),
    }
}

fn credential(account: &Account) -> Credential {
    Credential {
        username: account.extension.clone(),
        password: account.password.clone(),
        realm: None,
    }
}

fn audio_error_text(err: &str) -> String {
    format!(
        "Нет доступа к микрофону или динамику. Разрешите доступ в настройках системы и попробуйте снова ({err})"
    )
}
