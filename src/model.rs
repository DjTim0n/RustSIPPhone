//! Общие типы: что умеет ядро телефона и о чём оно сообщает интерфейсу.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// Адрес станции вместе с портом, например `192.168.1.10:5060`.
    pub server: String,
    /// Внутренний номер (он же логин).
    pub extension: String,
    pub password: String,
}

/// Что интерфейс просит сделать ядро.
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
    /// Текст уже на человеческом языке. `retry` — ядро будет пробовать снова само.
    Failed { message: String, retry: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Incoming,
    Outgoing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Набираем, ответа от станции ещё нет.
    Dialing,
    /// Телефон абонента звонит.
    Ringing,
    /// Нам звонят, ждём решения.
    Incoming,
    /// Разговор идёт.
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
    /// Начало звонка, секунды с 1970 года.
    pub started_at: i64,
    pub duration_secs: u64,
}

/// О чём ядро сообщает интерфейсу.
#[derive(Debug)]
pub enum Event {
    Reg(RegState),
    Call(Option<CallView>),
    CallEnded {
        entry: HistoryEntry,
        /// Что показать человеку, если звонок закончился не разговором.
        message: Option<String>,
    },
    Toast(String),
}

/// Отправка событий интерфейсу с пробуждением его отрисовки.
#[derive(Clone)]
pub struct Events {
    tx: std::sync::mpsc::Sender<Event>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Events {
    pub fn new(
        tx: std::sync::mpsc::Sender<Event>,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
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

/// Понятное объяснение, почему станция отклонила вызов.
pub fn describe_call_status(code: u16) -> String {
    match code {
        404 | 604 => "Такого номера нет".into(),
        480 | 503 =>"Абонент сейчас недоступен".into(),
        486 | 600 => "Абонент занят".into(),
        603 | 487 => "Абонент сбросил вызов".into(),
        401 | 403 | 407 => "Станция не разрешила этот звонок".into(),
        408 | 504 => "Станция не отвечает".into(),
        _ => format!("Не удалось дозвониться (код {code})"),
    }
}

/// Понятное объяснение, почему не удалось войти.
pub fn describe_register_status(code: u16) -> String {
    match code {
        401 | 403 | 407 => "Станция не приняла номер или пароль. Проверьте и попробуйте снова".into(),
        404 => "Станция не знает такой номер".into(),
        _ => format!("Станция ответила отказом (код {code})"),
    }
}
