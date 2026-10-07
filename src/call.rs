//! Calls: outgoing (INVITE) and incoming, the conversation with RTP media, and teardown.

use crate::audio::AudioIo;
use crate::media::{self, MediaControl};
use crate::model::*;
use crate::ringtone::Ringtone;
use crate::sdp;
use crate::settings::AudioSettings;
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

/// How long the phone rings before an unanswered incoming call is given up.
const INCOMING_RING_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug)]
pub enum CallCtl {
    Answer,
    Reject,
    Hangup,
    Mute(bool),
    Dtmf(char),
}

/// We never have more than one call at a time.
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
    /// Addresses of the station (and of the outbound proxy, if any); media and calls from them
    /// are trusted.
    pub server_ips: Arc<Vec<IpAddr>>,
    /// What registration learned about how the station sees us.
    pub link: SharedLink,
    pub audio: Arc<Mutex<AudioSettings>>,
    pub events: Events,
    pub slot: Arc<CallSlot>,
}

/// What registration learned that calls need as well.
#[derive(Clone, Debug)]
pub struct LinkInfo {
    /// Our Contact address as the station sees it (public address, transport).
    pub contact: rsipstack::sip::Uri,
    /// The public IP to advertise for media, if one was discovered and may be used.
    pub public_ip: Option<IpAddr>,
}

pub type SharedLink = Arc<Mutex<Option<LinkInfo>>>;

impl CallContext {
    fn link(&self) -> Option<LinkInfo> {
        self.link.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The IP address put into the SDP: the public one when the station reported it.
    fn media_ip(&self) -> IpAddr {
        self.link()
            .and_then(|link| link.public_ip)
            .unwrap_or(self.local_ip)
    }

    fn audio_settings(&self) -> AudioSettings {
        self.audio.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

struct Finished {
    outcome: Outcome,
    notice: Option<Notice>,
    connected_at: Option<Instant>,
}

impl Finished {
    fn failed(notice: Notice) -> Self {
        Finished {
            outcome: Outcome::Failed,
            notice: Some(notice),
            connected_at: None,
        }
    }

    fn without_talk(outcome: Outcome, notice: Option<Notice>) -> Self {
        Finished {
            outcome,
            notice,
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
        notice: done.notice,
    });
    ctx.events.send(Event::Call(None));
}

// ---------------------------------------------------------------- outgoing

pub fn start_outgoing(ctx: CallContext, number: String) -> Result<(), Notice> {
    let Some((ctl_rx, guard)) = ctx.slot.claim() else {
        return Err(Notice::FinishCurrentCall);
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

    // Open the devices before the INVITE: if microphone access is missing, better to learn it right away.
    let audio = match AudioIo::start(&ctx.audio_settings()) {
        Ok(audio) => audio,
        Err(err) => return Finished::failed(Notice::AudioUnavailable(err)),
    };
    let rtp_socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(socket) => Arc::new(socket),
        Err(err) => return Finished::failed(Notice::SoundSetupFailed(err.to_string())),
    };
    let rtp_port = rtp_socket.local_addr().map(|a| a.port()).unwrap_or(0);
    let offer = sdp::build_offer(ctx.media_ip(), rtp_port, now_unix() as u64);

    let parse_uri = |text: String| {
        text.parse::<rsipstack::sip::Uri>()
            .map_err(|e| e.to_string())
    };
    let build_invite = || -> Result<InviteOption, String> {
        let contact = match ctx.link() {
            Some(link) => link.contact,
            None => ctx
                .dialog_layer
                .build_local_contact(Some(account.extension.clone()), None)
                .map_err(|e| e.to_string())?,
        };
        let display_name = account.connection.display_name.trim();
        Ok(InviteOption {
            caller_display_name: (!display_name.is_empty()).then(|| display_name.to_string()),
            caller: parse_uri(account.own_uri())?,
            callee: parse_uri(account.callee_uri(number))?,
            contact,
            content_type: Some("application/sdp".to_string()),
            offer: Some(offer.clone().into_bytes()),
            credential: Some(credential(account)),
            ..Default::default()
        })
    };
    let invite = match build_invite() {
        Ok(invite) => invite,
        Err(_) => {
            return Finished::failed(Notice::CannotDial);
        }
    };

    let (state_tx, mut state_rx) = ctx.dialog_layer.new_dialog_state_channel();
    let invite_future = ctx.dialog_layer.do_invite(invite, state_tx);
    tokio::pin!(invite_future);

    let (dialog, response) = loop {
        tokio::select! {
            result = &mut invite_future => match result {
                Ok(pair) => break pair,
                Err(_) => return Finished::failed(Notice::ServerNotResponding),
            },
            Some(state) = state_rx.recv() => {
                if matches!(state, DialogState::Early(..)) {
                    publish(ctx, number, Phase::Ringing);
                }
            }
            ctl = ctl_rx.recv() => match ctl {
                // Dropping the future makes rsipstack send CANCEL itself.
                Some(CallCtl::Hangup | CallCtl::Reject) | None => {
                    return Finished::without_talk(Outcome::Cancelled, None);
                }
                _ => {}
            },
        }
    };

    let Some(response) = response else {
        return Finished::failed(Notice::NoAnswerFromServer);
    };
    if response.status_code.kind() != StatusCodeKind::Successful {
        let code = response.status_code.code();
        return Finished::without_talk(Outcome::Failed, Some(Notice::CallRejected(code)));
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
            Finished::failed(Notice::SoundNegotiationFailed(err))
        }
    };
    ctx.dialog_layer.remove_dialog(&dialog.id());
    result
}

fn response_body(response: &rsipstack::sip::Response) -> Result<String, String> {
    String::from_utf8(response.body().to_vec()).map_err(|_| "the response is not UTF-8".to_string())
}

// ----------------------------------------------------------------- incoming

pub fn start_incoming(ctx: CallContext, tx: Transaction) {
    let Some((ctl_rx, guard)) = ctx.slot.claim() else {
        // Already on a call: tell the caller we are busy.
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
        return UNKNOWN_PEER.to_string();
    };
    let number = from
        .uri
        .auth
        .as_ref()
        .map(|auth| auth.user.clone())
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| from.uri.host_with_port.to_string());
    match from
        .display_name
        .as_deref()
        .map(|n| n.trim_matches('"').trim())
    {
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
        .map_err(|_| "the request has no audio offer".to_string())
        .and_then(|body| sdp::parse_remote(&body))
    {
        Ok(remote) => remote,
        Err(_) => {
            let _ = tx.reply(StatusCode::NotAcceptableHere).await;
            return Finished::failed(Notice::IncomingNoCommonAudio(peer.to_string()));
        }
    };

    let (state_tx, mut state_rx) = ctx.dialog_layer.new_dialog_state_channel();
    let dialog = match ctx.dialog_layer.get_or_create_server_invite(
        &tx,
        state_tx,
        Some(credential(&ctx.account)),
        ctx.link().map(|link| link.contact),
    ) {
        Ok(dialog) => dialog,
        Err(_) => {
            let _ = tx.reply(StatusCode::ServerInternalError).await;
            return Finished::failed(Notice::IncomingFailed);
        }
    };
    // The handler waits for the caller's ACK or CANCEL and moves the dialog state along.
    let mut handler_dialog = dialog.clone();
    tokio::spawn(async move {
        let _ = handler_dialog.handle(&mut tx).await;
    });
    let _ = dialog.ringing(None, None);

    publish(ctx, peer, Phase::Incoming);
    // No speaker, or the tone failed to start: the call is still visible on screen.
    let output_device = ctx.audio_settings().output_device;
    let ringtone = tokio::task::spawn_blocking(move || Ringtone::start(output_device.as_deref()))
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
            return Finished::without_talk(Outcome::Missed, Some(Notice::Missed(peer.to_string())));
        }
        Err(_) => {
            let _ = dialog.reject(Some(StatusCode::TemporarilyUnavailable), None);
            ctx.dialog_layer.remove_dialog(&dialog.id());
            return Finished::without_talk(Outcome::Missed, Some(Notice::Missed(peer.to_string())));
        }
    }

    let audio = match AudioIo::start(&ctx.audio_settings()) {
        Ok(audio) => audio,
        Err(err) => {
            let _ = dialog.reject(Some(StatusCode::ServerInternalError), None);
            ctx.dialog_layer.remove_dialog(&dialog.id());
            return Finished::failed(Notice::AudioUnavailable(err));
        }
    };
    let rtp_socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(socket) => Arc::new(socket),
        Err(err) => {
            let _ = dialog.reject(Some(StatusCode::ServerInternalError), None);
            ctx.dialog_layer.remove_dialog(&dialog.id());
            return Finished::failed(Notice::SoundSetupFailed(err.to_string()));
        }
    };
    let rtp_port = rtp_socket.local_addr().map(|a| a.port()).unwrap_or(0);
    let answer = sdp::build_sdp(
        ctx.media_ip(),
        rtp_port,
        now_unix() as u64,
        &[remote.codec],
        remote.dtmf_pt,
    );
    let headers = vec![rsipstack::sip::Header::ContentType(
        "application/sdp".into(),
    )];
    if dialog
        .accept(Some(headers), Some(answer.into_bytes()))
        .is_err()
    {
        ctx.dialog_layer.remove_dialog(&dialog.id());
        return Finished::failed(Notice::AnswerFailed);
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

// ----------------------------------------------------------------- conversation

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
        ctx.server_ips.to_vec(),
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
                    break Some(Notice::PeerEndedCall);
                }
                Some(_) => {}
            },
        }
    };

    stop.cancel();
    let stats = media_task.await.unwrap_or_default();
    // If no audio packet arrived for a noticeable time, the network is most likely blocking it.
    let silent = stats.received == 0 && connected_at.elapsed() > Duration::from_secs(5);
    let notice = match (message, silent) {
        (_, true) => Some(Notice::NoAudioReceived),
        (message, false) => message,
    };
    Finished {
        outcome: Outcome::Completed,
        notice,
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
