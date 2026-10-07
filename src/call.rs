//! Calls: outgoing (INVITE) and incoming, the conversation with RTP media, and teardown.

use crate::audio::AudioIo;
use crate::media::{self, MediaControl};
use crate::model::*;
use crate::ringtone::{Ringtone, Tone};
use crate::sdp;
use crate::settings::{AudioSettings, CallSettings};
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
/// How long the phone rings before it answers by itself, when auto-answer is on.
const AUTO_ANSWER_AFTER: Duration = Duration::from_millis(1500);

#[derive(Debug)]
pub enum CallCtl {
    Answer,
    Reject,
    Hangup,
    Mute(bool),
    Dtmf(char),
    /// Hold (`true`) or resume (`false`) the call.
    Hold(bool),
    /// Hand the call over to this number or address.
    Transfer(String),
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
    pub calls: Arc<Mutex<CallSettings>>,
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

    fn calls(&self) -> CallSettings {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
    ctx.events
        .send(Event::Call(Some(CallView::new(peer, phase))));
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
    let session_id = now_unix() as u64;
    let offer = sdp::build_offer(ctx.media_ip(), rtp_port, session_id);

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

    // While the other phone rings we either hear the station's own sound (early media) or, if it
    // sends none, a ringback tone of our own.
    let mut audio = Some(audio);
    let mut early: Option<RunningMedia> = None;
    let mut ringback: Option<Ringtone> = None;

    let (dialog, response) = loop {
        tokio::select! {
            result = &mut invite_future => match result {
                Ok(pair) => break pair,
                Err(_) => return Finished::failed(Notice::ServerNotResponding),
            },
            Some(state) = state_rx.recv() => {
                if let DialogState::Early(_, provisional) = &state {
                    publish(ctx, number, Phase::Ringing);
                    if early.is_none() {
                        match early_remote(provisional) {
                            Some(remote) => {
                                if let Some(audio) = audio.take() {
                                    ringback = None;
                                    early = Some(start_media(ctx, rtp_socket.clone(), audio, remote, true, session_id));
                                }
                            }
                            None if ringback.is_none() => ringback = start_ringback(ctx).await,
                            None => {}
                        }
                    }
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

    drop(ringback);
    let result = match response_body(&response).and_then(|body| sdp::parse_remote(&body)) {
        Ok(remote) => {
            let media = match (early.take(), audio.take()) {
                // The station's early media already runs to the right place: just turn the
                // microphone on.
                (Some(media), _) if media.remote == remote => Some(media),
                // It changed its mind about where the audio goes: start over with fresh devices.
                (Some(stale), _) => {
                    drop(stale);
                    AudioIo::start(&ctx.audio_settings()).ok().map(|audio| {
                        start_media(
                            ctx,
                            rtp_socket.clone(),
                            audio,
                            remote.clone(),
                            false,
                            session_id,
                        )
                    })
                }
                (None, Some(audio)) => Some(start_media(
                    ctx,
                    rtp_socket.clone(),
                    audio,
                    remote,
                    false,
                    session_id,
                )),
                (None, None) => None,
            };
            match media {
                Some(media) => talk(ctx, &dialog, number, media, &mut state_rx, &mut ctl_rx).await,
                None => {
                    let _ = dialog.bye().await;
                    Finished::failed(Notice::SoundSetupFailed("no audio device".into()))
                }
            }
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

    // Do not disturb: turn the call away at once. It still shows up as a missed call.
    if ctx.calls().do_not_disturb {
        let _ = tx.reply(StatusCode::BusyHere).await;
        return Finished::without_talk(Outcome::Missed, None);
    }

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
    let ringtone =
        tokio::task::spawn_blocking(move || Ringtone::start(output_device.as_deref(), Tone::Ring))
            .await
            .ok()
            .and_then(Result::ok);

    // With auto-answer the phone picks up by itself after a moment.
    let auto_answer = ctx.calls().auto_answer;
    let auto_answer_timer = tokio::time::sleep(AUTO_ANSWER_AFTER);
    tokio::pin!(auto_answer_timer);
    let decision = tokio::time::timeout(INCOMING_RING_TIMEOUT, async {
        loop {
            tokio::select! {
                _ = &mut auto_answer_timer, if auto_answer => return Decision::Answer,
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
    let session_id = now_unix() as u64;
    let answer = sdp::build_sdp(&sdp::LocalSdp {
        ip: ctx.media_ip(),
        rtp_port,
        session_id,
        version: session_id,
        codecs: &[remote.codec],
        dtmf_pt: remote.dtmf_pt,
        direction: sdp::MediaDirection::SendRecv,
    });
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

    let media = start_media(ctx, rtp_socket, audio, remote, false, session_id);
    let result = talk(ctx, &dialog, peer, media, &mut state_rx, &mut ctl_rx).await;
    ctx.dialog_layer.remove_dialog(&dialog.id());
    result
}

enum Decision {
    Answer,
    Decline,
    CallerGaveUp,
}

// ----------------------------------------------------------------- media

/// The audio of a call: it starts as soon as the other side sends sound (early media, when the
/// station plays its own ringback tone or music while the phone is still ringing) and goes on as
/// the conversation. Dropping it stops the audio and closes the devices.
struct RunningMedia {
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<media::MediaStats>>,
    /// While true the microphone is replaced by silence.
    muted: Arc<AtomicBool>,
    dtmf: UnboundedSender<char>,
    remote: sdp::Remote,
    /// Our RTP port and the id of the session description we sent, which re-offers must reuse.
    local_port: u16,
    session_id: u64,
}

impl Drop for RunningMedia {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Starts sending and receiving audio. `muted` keeps the microphone off, which is what the phone
/// does until the other side has answered: audio is received and silence is sent.
fn start_media(
    ctx: &CallContext,
    rtp_socket: Arc<UdpSocket>,
    audio: AudioIo,
    remote: sdp::Remote,
    muted: bool,
    session_id: u64,
) -> RunningMedia {
    let local_port = rtp_socket.local_addr().map(|a| a.port()).unwrap_or(0);
    let muted = Arc::new(AtomicBool::new(muted));
    let (dtmf_tx, dtmf_rx) = unbounded_channel();
    let stop = CancellationToken::new();
    let task = tokio::spawn(media::run(
        rtp_socket,
        remote.clone(),
        audio,
        MediaControl {
            muted: muted.clone(),
            dtmf: dtmf_rx,
        },
        ctx.server_ips.to_vec(),
        stop.clone(),
    ));
    RunningMedia {
        stop,
        task: Some(task),
        muted,
        dtmf: dtmf_tx,
        remote,
        local_port,
        session_id,
    }
}

/// Where early media comes from, if this provisional response carries a session description.
fn early_remote(response: &rsipstack::sip::Response) -> Option<sdp::Remote> {
    if response.body().is_empty() {
        return None;
    }
    response_body(response)
        .and_then(|body| sdp::parse_remote(&body))
        .ok()
}

/// Plays the ringback tone on the chosen speaker. No speaker or a failure means silence, which is
/// better than failing the call.
async fn start_ringback(ctx: &CallContext) -> Option<Ringtone> {
    let output_device = ctx.audio_settings().output_device;
    tokio::task::spawn_blocking(move || Ringtone::start(output_device.as_deref(), Tone::Ringback))
        .await
        .ok()
        .and_then(Result::ok)
}

// ----------------------------------------------------------------- conversation

/// How long a transfer may stay unconfirmed before the phone stops waiting.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(20);

/// A transfer that was requested and is waiting for the station's confirmation.
struct PendingTransfer {
    target: String,
    since: Instant,
}

/// Mutable facts about the call that the screen shows.
struct CallFlags {
    user_muted: bool,
    view: CallView,
    /// The version of the next session description we send; it must keep growing.
    sdp_version: u64,
    transfer: Option<PendingTransfer>,
}

impl CallFlags {
    fn publish(&self, ctx: &CallContext) {
        ctx.events.send(Event::Call(Some(self.view.clone())));
    }

    /// The microphone is off while muted by the user or while the call is on hold.
    fn apply_mute(&self, media: &RunningMedia) {
        media
            .muted
            .store(self.user_muted || self.view.local_hold, Ordering::Relaxed);
    }
}

async fn talk(
    ctx: &CallContext,
    dialog: &InviteDialog,
    peer: &str,
    mut media: RunningMedia,
    state_rx: &mut UnboundedReceiver<DialogState>,
    ctl_rx: &mut UnboundedReceiver<CallCtl>,
) -> Finished {
    let connected_at = Instant::now();
    let mut flags = CallFlags {
        user_muted: false,
        view: CallView {
            connected_at: Some(connected_at),
            ..CallView::new(peer, Phase::Active)
        },
        sdp_version: media.session_id,
        transfer: None,
    };
    flags.publish(ctx);
    // The call is answered: from now on the other side hears us.
    flags.apply_mute(&media);
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    let message = loop {
        tokio::select! {
            ctl = ctl_rx.recv() => match ctl {
                Some(CallCtl::Hangup | CallCtl::Reject) | None => {
                    let _ = dialog.bye().await;
                    break None;
                }
                Some(CallCtl::Mute(on)) => {
                    flags.user_muted = on;
                    flags.apply_mute(&media);
                }
                Some(CallCtl::Dtmf(digit)) => send_dtmf(ctx, dialog, &media, digit).await,
                Some(CallCtl::Hold(on)) => {
                    flags.sdp_version += 1;
                    let direction = if on { sdp::MediaDirection::SendOnly } else { sdp::MediaDirection::SendRecv };
                    match renegotiate(ctx, dialog, &media, flags.sdp_version, direction).await {
                        Ok(()) => {
                            flags.view.local_hold = on;
                            flags.apply_mute(&media);
                            flags.publish(ctx);
                        }
                        Err(code) => ctx.events.send(Event::Toast(Notice::HoldFailed(code))),
                    }
                }
                Some(CallCtl::Transfer(target)) => {
                    match request_transfer(ctx, dialog, &target).await {
                        Ok(()) => {
                            flags.transfer = Some(PendingTransfer { target, since: Instant::now() });
                            flags.view.transferring = true;
                            flags.publish(ctx);
                        }
                        Err(code) => ctx.events.send(Event::Toast(Notice::TransferFailed(code))),
                    }
                }
                Some(CallCtl::Answer) => {}
            },
            state = state_rx.recv() => match state {
                Some(DialogState::Terminated(..)) | None => {
                    break Some(Notice::PeerEndedCall);
                }
                Some(DialogState::Notify(_, request, handle)) => {
                    let _ = handle.reply(rsipstack::sip::StatusCode::OK).await;
                    if let Some(code) = sipfrag_status(&request) {
                        if let Some(done) = flags.transfer_progress(ctx, code) {
                            let _ = dialog.bye().await;
                            break Some(done);
                        }
                    }
                }
                Some(state) => answer_in_dialog(ctx, state, &media, &mut flags).await,
            },
            _ = tick.tick() => {
                if flags.transfer.as_ref().is_some_and(|t| t.since.elapsed() > TRANSFER_TIMEOUT) {
                    flags.transfer = None;
                    flags.view.transferring = false;
                    flags.publish(ctx);
                    ctx.events.send(Event::Toast(Notice::TransferUnconfirmed));
                }
            }
        }
    };

    media.stop.cancel();
    let stats = match media.task.take() {
        Some(task) => task.await.unwrap_or_default(),
        None => media::MediaStats::default(),
    };
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

impl CallFlags {
    /// Reacts to a progress report on a transfer (the status code in a NOTIFY). Returns the
    /// notice to end the call with when the transfer has gone through.
    fn transfer_progress(&mut self, ctx: &CallContext, code: u16) -> Option<Notice> {
        let pending = self.transfer.as_ref()?;
        match code {
            // 1xx: still trying.
            100..=199 => None,
            200..=299 => {
                let target = pending.target.clone();
                self.transfer = None;
                Some(Notice::Transferred(target))
            }
            _ => {
                self.transfer = None;
                self.view.transferring = false;
                self.publish(ctx);
                ctx.events.send(Event::Toast(Notice::TransferFailed(code)));
                None
            }
        }
    }
}

/// Sends a key to the other side the way the settings and the other side's abilities allow.
async fn send_dtmf(ctx: &CallContext, dialog: &InviteDialog, media: &RunningMedia, digit: char) {
    let tones_negotiated = media.remote.dtmf_pt.is_some();
    if ctx.calls().dtmf_mode.use_audio_tones(tones_negotiated) {
        let _ = media.dtmf.send(digit);
    } else {
        let headers = vec![rsipstack::sip::Header::ContentType(
            "application/dtmf-relay".into(),
        )];
        let _ = dialog
            .info(Some(headers), Some(dtmf_relay_body(digit).into_bytes()))
            .await;
    }
}

/// The body of a SIP INFO that carries a key press (the common `application/dtmf-relay` form).
fn dtmf_relay_body(digit: char) -> String {
    format!("Signal={digit}\r\nDuration=160\r\n")
}

/// Sends a new session description in the call (a re-INVITE): this is how a call is put on hold
/// and taken off hold. Returns the station's status code if it refuses.
async fn renegotiate(
    ctx: &CallContext,
    dialog: &InviteDialog,
    media: &RunningMedia,
    version: u64,
    direction: sdp::MediaDirection,
) -> Result<(), u16> {
    let offer = sdp::build_sdp(&sdp::LocalSdp {
        ip: ctx.media_ip(),
        rtp_port: media.local_port,
        session_id: media.session_id,
        version,
        codecs: &[media.remote.codec],
        dtmf_pt: media.remote.dtmf_pt,
        direction,
    });
    let headers = vec![rsipstack::sip::Header::ContentType(
        "application/sdp".into(),
    )];
    match dialog
        .reinvite(Some(headers), Some(offer.into_bytes()))
        .await
    {
        Ok(Some(response)) if response.status_code.kind() == StatusCodeKind::Successful => Ok(()),
        Ok(Some(response)) => Err(response.status_code.code()),
        Ok(None) | Err(_) => Err(0),
    }
}

/// Asks the station to hand the call over to `target` (a blind transfer). Success means only that
/// the request was accepted; the outcome follows in NOTIFY messages.
async fn request_transfer(
    ctx: &CallContext,
    dialog: &InviteDialog,
    target: &str,
) -> Result<(), u16> {
    let uri = ctx
        .account
        .callee_uri(target)
        .parse::<rsipstack::sip::Uri>()
        .map_err(|_| 0u16)?;
    match dialog.refer(uri, None, None).await {
        Ok(Some(response)) if response.status_code.kind() == StatusCodeKind::Successful => Ok(()),
        Ok(Some(response)) => Err(response.status_code.code()),
        Ok(None) | Err(_) => Err(0),
    }
}

/// The status line of a transfer progress report: the body of the NOTIFY is a fragment of a SIP
/// response such as `SIP/2.0 200 OK`.
fn sipfrag_status(request: &rsipstack::sip::Request) -> Option<u16> {
    let body = std::str::from_utf8(request.body()).ok()?;
    let mut words = body.split_whitespace();
    if !words.next()?.starts_with("SIP/2.0") {
        return None;
    }
    words.next()?.parse().ok()
}

/// Answers a request the station sent inside the call: a new session description (hold, resume,
/// refresh), a key press, a ping, a message. The stack waits for the answer and would otherwise
/// reply "not implemented" after half a minute.
async fn answer_in_dialog(
    ctx: &CallContext,
    state: DialogState,
    media: &RunningMedia,
    flags: &mut CallFlags,
) {
    use rsipstack::sip::StatusCode;
    match state {
        DialogState::Updated(_, request, handle) => {
            let body = std::str::from_utf8(request.body()).unwrap_or_default();
            if body.trim().is_empty() {
                // A session refresh without a new description.
                let _ = handle.reply(StatusCode::OK).await;
                return;
            }
            match sdp::parse_remote(body) {
                Ok(remote) => {
                    let held = remote.on_hold();
                    if flags.view.remote_hold != held {
                        flags.view.remote_hold = held;
                        flags.publish(ctx);
                    }
                    flags.sdp_version += 1;
                    let answer = sdp::build_sdp(&sdp::LocalSdp {
                        ip: ctx.media_ip(),
                        rtp_port: media.local_port,
                        session_id: media.session_id,
                        version: flags.sdp_version,
                        codecs: &[media.remote.codec],
                        dtmf_pt: media.remote.dtmf_pt,
                        direction: remote.direction.answer(),
                    });
                    let headers = vec![rsipstack::sip::Header::ContentType(
                        "application/sdp".into(),
                    )];
                    let _ = handle
                        .respond(StatusCode::OK, Some(headers), Some(answer.into_bytes()))
                        .await;
                }
                Err(_) => {
                    let _ = handle.reply(StatusCode::NotAcceptableHere).await;
                }
            }
        }
        DialogState::Info(_, _, handle)
        | DialogState::Options(_, _, handle)
        | DialogState::Message(_, _, handle) => {
            let _ = handle.reply(StatusCode::OK).await;
        }
        // Being transferred by the other side is not supported yet: say so at once.
        DialogState::Refer(_, _, handle) | DialogState::Publish(_, _, handle) => {
            let _ = handle.reply(StatusCode::NotImplemented).await;
        }
        _ => {}
    }
}

fn credential(account: &Account) -> Credential {
    Credential {
        username: account.extension.clone(),
        password: account.password.clone(),
        realm: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provisional(status: &str, body: &str) -> rsipstack::sip::Response {
        let raw = format!(
            "SIP/2.0 {status}\r\nVia: SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bK1\r\n\
             From: <sip:300@x>;tag=1\r\nTo: <sip:100@x>;tag=2\r\nCall-ID: a\r\nCSeq: 1 INVITE\r\n\
             Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        rsipstack::sip::Response::try_from(raw.as_str()).expect("valid response")
    }

    #[test]
    fn a_183_with_sdp_means_the_station_sends_early_media() {
        let sdp = "v=0\r\nc=IN IP4 203.0.113.9\r\nm=audio 20000 RTP/AVP 8 101\r\n\
                   a=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\n";
        let remote = early_remote(&provisional("183 Session Progress", sdp))
            .expect("early media is announced");
        assert_eq!(remote.addr, "203.0.113.9:20000".parse().unwrap());
        assert_eq!(remote.codec, crate::g711::Codec::Pcma);
    }

    #[test]
    fn a_plain_180_means_the_phone_plays_its_own_ringback() {
        assert!(early_remote(&provisional("180 Ringing", "")).is_none());
    }

    #[test]
    fn unusable_sdp_does_not_count_as_early_media() {
        // No common codec: better to fall back to our own ringback tone than to hear nothing.
        let sdp = "v=0\r\nc=IN IP4 203.0.113.9\r\nm=audio 20000 RTP/AVP 18\r\n";
        assert!(early_remote(&provisional("183 Session Progress", sdp)).is_none());
    }

    // ---- requests the station sends inside a call

    use rsipstack::dialog::DialogId;
    use rsipstack::dialog::dialog::{TransactionCommand, TransactionHandle};
    use rsipstack::sip::StatusCode;

    fn dialog_id() -> DialogId {
        DialogId {
            call_id: "call-1".into(),
            local_tag: "local".into(),
            remote_tag: "remote".into(),
        }
    }

    fn request(method: &str, body: &str) -> rsipstack::sip::Request {
        let raw = format!(
            "{method} sip:300@10.0.0.5 SIP/2.0\r\nVia: SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bK9\r\n\
             From: <sip:100@x>;tag=remote\r\nTo: <sip:300@x>;tag=local\r\nCall-ID: call-1\r\n\
             CSeq: 2 {method}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        rsipstack::sip::Request::try_from(raw.as_str()).expect("valid request")
    }

    /// A call context that is good enough to answer requests with. It has no network behind it.
    fn context() -> (CallContext, std::sync::mpsc::Receiver<Event>) {
        let endpoint = rsipstack::EndpointBuilder::new().build();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = CallContext {
            dialog_layer: Arc::new(DialogLayer::new(endpoint.inner.clone())),
            account: Arc::new(Account {
                server: "pbx.example.com".into(),
                extension: "300".into(),
                password: "secret".into(),
                connection: Default::default(),
            }),
            local_ip: "10.0.0.5".parse().unwrap(),
            server_ips: Arc::new(vec!["10.0.0.2".parse().unwrap()]),
            link: Arc::new(Mutex::new(None)),
            audio: Arc::new(Mutex::new(AudioSettings::default())),
            calls: Arc::new(Mutex::new(CallSettings::default())),
            events: Events::new(tx, Arc::new(|| {})),
            slot: Arc::new(CallSlot::default()),
        };
        (ctx, rx)
    }

    fn media() -> RunningMedia {
        let (dtmf, _rx) = unbounded_channel();
        RunningMedia {
            stop: CancellationToken::new(),
            task: None,
            muted: Arc::new(AtomicBool::new(false)),
            dtmf,
            remote: sdp::Remote {
                addr: "10.0.0.2:20000".parse().unwrap(),
                codec: crate::g711::Codec::Pcma,
                dtmf_pt: Some(101),
                direction: sdp::MediaDirection::SendRecv,
            },
            local_port: 40000,
            session_id: 77,
        }
    }

    fn flags() -> CallFlags {
        CallFlags {
            user_muted: false,
            view: CallView::new("100", Phase::Active),
            sdp_version: 77,
            transfer: None,
        }
    }

    /// Feeds `state` to the call and returns what was answered, plus the new flags.
    async fn answer(
        make_state: impl FnOnce(TransactionHandle) -> DialogState,
    ) -> (
        Option<(StatusCode, Option<Vec<u8>>)>,
        CallFlags,
        std::sync::mpsc::Receiver<Event>,
    ) {
        let (ctx, events) = context();
        let (handle, mut replies) = TransactionHandle::new();
        let mut flags = flags();
        answer_in_dialog(&ctx, make_state(handle), &media(), &mut flags).await;
        let reply = replies.try_recv().ok().map(|command| match command {
            TransactionCommand::Respond { status, body, .. } => (status, body),
        });
        (reply, flags, events)
    }

    #[tokio::test]
    async fn a_hold_request_is_answered_recvonly_and_shown() {
        let sdp = "v=0\r\nc=IN IP4 10.0.0.2\r\nm=audio 20000 RTP/AVP 8\r\na=sendonly\r\n";
        let (reply, flags, events) =
            answer(|h| DialogState::Updated(dialog_id(), request("INVITE", sdp), h)).await;
        let (status, body) = reply.expect("the request is answered");
        assert_eq!(status, StatusCode::OK);
        let answer = String::from_utf8(body.expect("with a description")).unwrap();
        assert!(answer.contains("a=recvonly"), "{answer}");
        assert!(answer.contains("m=audio 40000"), "our own port: {answer}");
        assert!(flags.view.remote_hold, "the screen is told");
        assert!(matches!(events.try_recv(), Ok(Event::Call(Some(view))) if view.remote_hold));
    }

    #[tokio::test]
    async fn resuming_clears_the_hold() {
        let sdp = "v=0\r\nc=IN IP4 10.0.0.2\r\nm=audio 20000 RTP/AVP 8\r\na=sendrecv\r\n";
        let (ctx, _events) = context();
        let (handle, mut replies) = TransactionHandle::new();
        let mut flags = flags();
        flags.view.remote_hold = true;
        let state = DialogState::Updated(dialog_id(), request("INVITE", sdp), handle);
        answer_in_dialog(&ctx, state, &media(), &mut flags).await;
        assert!(!flags.view.remote_hold);
        assert!(replies.try_recv().is_ok());
    }

    #[tokio::test]
    async fn the_version_of_our_descriptions_keeps_growing() {
        let sdp = "v=0\r\nc=IN IP4 10.0.0.2\r\nm=audio 20000 RTP/AVP 8\r\na=sendrecv\r\n";
        let (reply, flags, _events) =
            answer(|h| DialogState::Updated(dialog_id(), request("INVITE", sdp), h)).await;
        let body = String::from_utf8(reply.unwrap().1.unwrap()).unwrap();
        assert!(body.contains("o=rustphone 77 78 "), "{body}");
        assert_eq!(flags.sdp_version, 78);
    }

    #[tokio::test]
    async fn a_refresh_without_a_description_gets_a_plain_ok() {
        let (reply, _flags, _events) =
            answer(|h| DialogState::Updated(dialog_id(), request("UPDATE", ""), h)).await;
        assert_eq!(reply, Some((StatusCode::OK, None)));
    }

    #[tokio::test]
    async fn an_unusable_description_is_refused() {
        let sdp = "v=0\r\nc=IN IP4 10.0.0.2\r\nm=audio 20000 RTP/AVP 18\r\n";
        let (reply, _flags, _events) =
            answer(|h| DialogState::Updated(dialog_id(), request("INVITE", sdp), h)).await;
        assert_eq!(reply.map(|r| r.0), Some(StatusCode::NotAcceptableHere));
    }

    #[tokio::test]
    async fn info_options_and_messages_are_acknowledged() {
        for method in ["INFO", "OPTIONS", "MESSAGE"] {
            let (reply, _f, _e) = answer(|h| match method {
                "INFO" => DialogState::Info(dialog_id(), request("INFO", ""), h),
                "OPTIONS" => DialogState::Options(dialog_id(), request("OPTIONS", ""), h),
                _ => DialogState::Message(dialog_id(), request("MESSAGE", "hi"), h),
            })
            .await;
            assert_eq!(reply.map(|r| r.0), Some(StatusCode::OK), "{method}");
        }
    }

    #[tokio::test]
    async fn being_transferred_by_the_other_side_is_declined_at_once() {
        let (reply, _f, _e) =
            answer(|h| DialogState::Refer(dialog_id(), request("REFER", ""), h)).await;
        assert_eq!(reply.map(|r| r.0), Some(StatusCode::NotImplemented));
    }

    // ---- transfer progress and keypad tones

    #[test]
    fn transfer_progress_is_read_from_the_notify_body() {
        let notify = |body: &str| request("NOTIFY", body);
        assert_eq!(sipfrag_status(&notify("SIP/2.0 100 Trying\r\n")), Some(100));
        assert_eq!(sipfrag_status(&notify("SIP/2.0 200 OK\r\n")), Some(200));
        assert_eq!(sipfrag_status(&notify("SIP/2.0 486 Busy Here")), Some(486));
        assert_eq!(sipfrag_status(&notify("hello")), None);
        assert_eq!(sipfrag_status(&notify("")), None);
    }

    #[test]
    fn a_confirmed_transfer_ends_the_call_and_a_refused_one_does_not() {
        let (ctx, events) = context();
        let mut f = flags();
        f.transfer = Some(PendingTransfer {
            target: "200".into(),
            since: Instant::now(),
        });
        f.view.transferring = true;
        assert!(f.transfer_progress(&ctx, 180).is_none(), "still trying");
        assert!(f.transfer.is_some());
        assert_eq!(
            f.transfer_progress(&ctx, 200),
            Some(Notice::Transferred("200".into()))
        );

        let mut f = flags();
        f.transfer = Some(PendingTransfer {
            target: "200".into(),
            since: Instant::now(),
        });
        f.view.transferring = true;
        assert!(f.transfer_progress(&ctx, 486).is_none());
        assert!(
            !f.view.transferring && f.transfer.is_none(),
            "back to a normal call"
        );
        let toasts: Vec<_> = events.try_iter().collect();
        assert!(
            toasts
                .iter()
                .any(|e| matches!(e, Event::Toast(Notice::TransferFailed(486))))
        );
    }

    #[test]
    fn keypad_tones_for_info_use_the_dtmf_relay_format() {
        assert_eq!(dtmf_relay_body('5'), "Signal=5\r\nDuration=160\r\n");
        assert_eq!(dtmf_relay_body('#'), "Signal=#\r\nDuration=160\r\n");
    }
}
