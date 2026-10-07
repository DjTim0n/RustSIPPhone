//! Interface languages and the human-readable text of every message the core can report.

use crate::model::Notice;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    #[default]
    English,
    Russian,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::English, Lang::Russian];

    /// Picks the text for this language. Keeps both translations next to each other at the call site.
    pub fn t(self, english: &'static str, russian: &'static str) -> &'static str {
        match self {
            Lang::English => english,
            Lang::Russian => russian,
        }
    }

    /// The language's own name, as shown in the language switch.
    pub fn name(self) -> &'static str {
        match self {
            Lang::English => "English",
            Lang::Russian => "Русский",
        }
    }

    /// Short code for tight spaces.
    pub fn code(self) -> &'static str {
        match self {
            Lang::English => "EN",
            Lang::Russian => "RU",
        }
    }
}

impl Notice {
    pub fn text(&self, lang: Lang) -> String {
        match self {
            Notice::SignInFirst => lang
                .t("Sign in first", "Сначала войдите в аккаунт")
                .into(),
            Notice::FinishCurrentCall => lang
                .t(
                    "Finish the current call first",
                    "Сначала завершите текущий звонок",
                )
                .into(),
            Notice::ServerNotFound => lang
                .t(
                    "Couldn't find the station at this address. Check how it is written",
                    "Не удалось найти станцию по этому адресу. Проверьте, как он написан",
                )
                .into(),
            Notice::NoNetwork => lang
                .t("No network connection", "Нет подключения к сети")
                .into(),
            Notice::PhoneStartFailed(detail) => format!(
                "{}: {detail}",
                lang.t("Couldn't start the phone", "Не удалось запустить телефон")
            ),
            Notice::ServerAddressInvalid => lang
                .t(
                    "The station address is not valid",
                    "Адрес станции записан неверно",
                )
                .into(),
            Notice::RegistrationRetrying => lang
                .t(
                    "The station is not responding. Trying again",
                    "Станция не отвечает. Пробуем снова",
                )
                .into(),
            Notice::RegistrationRejected(code) => match code {
                401 | 403 | 407 => lang
                    .t(
                        "The station didn't accept the number or password. Check them and try again",
                        "Станция не приняла номер или пароль. Проверьте и попробуйте снова",
                    )
                    .into(),
                404 => lang
                    .t(
                        "The station doesn't know this number",
                        "Станция не знает такой номер",
                    )
                    .into(),
                _ => format!(
                    "{} ({} {code})",
                    lang.t("The station refused to sign you in", "Станция ответила отказом"),
                    lang.t("code", "код")
                ),
            },
            Notice::CallRejected(code) => match code {
                404 | 604 => lang
                    .t("There is no such number", "Такого номера нет")
                    .into(),
                480 | 503 => lang
                    .t(
                        "The person is unavailable right now",
                        "Абонент сейчас недоступен",
                    )
                    .into(),
                486 | 600 => lang.t("The line is busy", "Абонент занят").into(),
                603 | 487 => lang
                    .t("The call was declined", "Абонент сбросил вызов")
                    .into(),
                401 | 403 | 407 => lang
                    .t(
                        "The station didn't allow this call",
                        "Станция не разрешила этот звонок",
                    )
                    .into(),
                408 | 504 => lang
                    .t("The station is not responding", "Станция не отвечает")
                    .into(),
                _ => format!(
                    "{} ({} {code})",
                    lang.t("Couldn't connect the call", "Не удалось дозвониться"),
                    lang.t("code", "код")
                ),
            },
            Notice::AudioUnavailable(detail) => format!(
                "{} ({detail})",
                lang.t(
                    "No access to the microphone or speaker. Allow access in system settings and try again",
                    "Нет доступа к микрофону или динамику. Разрешите доступ в настройках системы и попробуйте снова",
                )
            ),
            Notice::SoundSetupFailed(detail) => format!(
                "{}: {detail}",
                lang.t("Couldn't set up audio", "Не удалось подготовить звук")
            ),
            Notice::CannotDial => lang
                .t(
                    "Couldn't dial this number. Check that it is entered correctly",
                    "Не удалось набрать этот номер. Проверьте, что он введён верно",
                )
                .into(),
            Notice::ServerNotResponding => lang
                .t(
                    "The station is not responding. Check your network connection",
                    "Станция не отвечает. Проверьте подключение к сети",
                )
                .into(),
            Notice::NoAnswerFromServer => lang
                .t(
                    "The station didn't answer the call",
                    "Станция не ответила на звонок",
                )
                .into(),
            Notice::SoundNegotiationFailed(detail) => format!(
                "{}: {detail}",
                lang.t(
                    "Couldn't agree on audio with the other side",
                    "Не удалось договориться о звуке",
                )
            ),
            Notice::IncomingNoCommonAudio(peer) => format!(
                "{} {peer}: {}",
                lang.t("Couldn't take the call from", "Не удалось принять звонок от"),
                lang.t("no common audio format", "нет общего способа передачи звука")
            ),
            Notice::IncomingFailed => lang
                .t(
                    "Couldn't take the incoming call",
                    "Не удалось принять входящий звонок",
                )
                .into(),
            Notice::Missed(peer) => format!("{}: {peer}", lang.t("Missed call", "Пропущенный звонок")),
            Notice::AnswerFailed => lang
                .t("Couldn't answer the call", "Не удалось ответить на звонок")
                .into(),
            Notice::PeerEndedCall => lang
                .t(
                    "The other person ended the call",
                    "Собеседник завершил разговор",
                )
                .into(),
            Notice::NoAudioReceived => lang
                .t(
                    "You couldn't hear the other person: no audio arrived. Network settings may be blocking it",
                    "Собеседника не было слышно: звук от него не приходил. Возможно, мешают настройки сети",
                )
                .into(),
            Notice::ConnectionFailed(detail) => format!(
                "{}: {detail}",
                lang.t(
                    "Couldn't connect to the station",
                    "Не удалось подключиться к станции"
                )
            ),
            Notice::TlsProblem(detail) => format!(
                "{}: {detail}",
                lang.t(
                    "The secure connection failed, check the station's certificate",
                    "Не удалось установить защищённое соединение, проверьте сертификат станции"
                )
            ),
            Notice::CertificateFileUnreadable(path) => format!(
                "{}: {path}",
                lang.t(
                    "Couldn't read the certificate file",
                    "Не удалось прочитать файл сертификата"
                )
            ),
            Notice::HoldFailed(code) => format!(
                "{} ({} {code})",
                lang.t(
                    "The station wouldn't put the call on hold",
                    "Станция не поставила звонок на удержание"
                ),
                lang.t("code", "код")
            ),
            Notice::TransferFailed(code) => format!(
                "{} ({} {code})",
                lang.t("The transfer was refused", "Перевод звонка отклонён"),
                lang.t("code", "код")
            ),
            Notice::TransferUnconfirmed => lang
                .t(
                    "The transfer was requested, but the station didn't confirm it",
                    "Перевод запрошен, но станция его не подтвердила",
                )
                .into(),
            Notice::Transferred(target) => format!(
                "{} {target}",
                lang.t("Call transferred to", "Звонок переведён на")
            ),
            Notice::PasswordNotSaved => lang
                .t(
                    "Password not saved: this system has no password store. You'll need to enter it next time",
                    "Пароль не сохранён: в системе нет хранилища паролей. В следующий раз его придётся ввести заново",
                )
                .into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_is_the_default() {
        assert_eq!(Lang::default(), Lang::English);
    }

    #[test]
    fn every_notice_has_text_in_both_languages() {
        let notices = [
            Notice::SignInFirst,
            Notice::FinishCurrentCall,
            Notice::ServerNotFound,
            Notice::NoNetwork,
            Notice::PhoneStartFailed("x".into()),
            Notice::ServerAddressInvalid,
            Notice::RegistrationRetrying,
            Notice::RegistrationRejected(403),
            Notice::RegistrationRejected(404),
            Notice::RegistrationRejected(500),
            Notice::CallRejected(486),
            Notice::CallRejected(404),
            Notice::CallRejected(999),
            Notice::AudioUnavailable("x".into()),
            Notice::SoundSetupFailed("x".into()),
            Notice::CannotDial,
            Notice::ServerNotResponding,
            Notice::NoAnswerFromServer,
            Notice::SoundNegotiationFailed("x".into()),
            Notice::IncomingNoCommonAudio("100".into()),
            Notice::IncomingFailed,
            Notice::Missed("100".into()),
            Notice::AnswerFailed,
            Notice::PeerEndedCall,
            Notice::NoAudioReceived,
            Notice::PasswordNotSaved,
            Notice::HoldFailed(488),
            Notice::TransferFailed(403),
            Notice::TransferUnconfirmed,
            Notice::Transferred("200".into()),
            Notice::ConnectionFailed("refused".into()),
            Notice::TlsProblem("unknown issuer".into()),
            Notice::CertificateFileUnreadable("/tmp/ca.pem".into()),
        ];
        for notice in notices {
            let en = notice.text(Lang::English);
            let ru = notice.text(Lang::Russian);
            assert!(!en.is_empty() && !ru.is_empty());
            assert!(
                !en.chars()
                    .any(|c| ('а'..='я').contains(&c.to_ascii_lowercase()) || c == 'ё'),
                "English text contains Cyrillic: {en}"
            );
            assert_ne!(en, ru);
        }
    }

    #[test]
    fn busy_line_is_described_per_language() {
        assert_eq!(
            Notice::CallRejected(486).text(Lang::English),
            "The line is busy"
        );
        assert_eq!(
            Notice::CallRejected(486).text(Lang::Russian),
            "Абонент занят"
        );
    }
}
