//! The settings screen: account, network, audio and general options.

use super::*;
use crate::audio::{self, AudioDevices};
use crate::settings::{
    AudioSettings, CallSettings, ConnectionSettings, DtmfMode, MAX_EXPIRY_SECS, MIN_EXPIRY_SECS,
    TransportKind,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsTab {
    Account,
    Network,
    Audio,
    General,
}

impl SettingsTab {
    const ALL: [SettingsTab; 4] = [
        SettingsTab::Account,
        SettingsTab::Network,
        SettingsTab::Audio,
        SettingsTab::General,
    ];

    fn label(self, l: Lang) -> &'static str {
        match self {
            SettingsTab::Account => l.t("Account", "Аккаунт"),
            SettingsTab::Network => l.t("Network", "Сеть"),
            SettingsTab::Audio => l.t("Audio", "Звук"),
            SettingsTab::General => l.t("General", "Основные"),
        }
    }
}

/// The values being edited. Nothing is applied until the user presses Save.
pub(super) struct SettingsDraft {
    pub tab: SettingsTab,
    pub server: String,
    pub extension: String,
    /// Empty means "keep the saved password".
    pub password: String,
    pub connection: ConnectionSettings,
    pub audio: AudioSettings,
    pub calls: CallSettings,
    /// The user tried to save without the station address or extension.
    pub missing: bool,
}

/// The project's home on GitHub, linked from the About card.
const REPOSITORY_URL: &str = "https://github.com/DjTim0n/RustSIPPhone";

/// Renewal times offered in the list, in seconds.
const EXPIRY_PRESETS: [u32; 7] = [30, 60, 120, 300, 600, 1800, 3600];

fn expiry_label(secs: u32, l: Lang) -> String {
    match secs {
        30 => l.t("30 seconds", "30 секунд").into(),
        60 => l.t("1 minute", "1 минута").into(),
        120 => l.t("2 minutes", "2 минуты").into(),
        300 => l.t("5 minutes", "5 минут").into(),
        600 => l.t("10 minutes", "10 минут").into(),
        1800 => l.t("30 minutes", "30 минут").into(),
        3600 => l.t("1 hour", "1 час").into(),
        other => format!("{other} {}", l.t("s", "с")),
    }
}

fn transport_label(transport: TransportKind, l: Lang) -> String {
    match transport {
        TransportKind::Udp | TransportKind::Tcp => transport.label().into(),
        TransportKind::Tls => format!("TLS ({})", l.t("encrypted", "шифрование")),
        TransportKind::Ws => "WebSocket (WS)".into(),
        TransportKind::Wss => format!("WebSocket (WSS, {})", l.t("encrypted", "шифрование")),
    }
}

fn dtmf_label(mode: DtmfMode, l: Lang) -> &'static str {
    match mode {
        DtmfMode::Auto => l.t("Automatic", "Автоматически"),
        DtmfMode::Rfc4733 => l.t("In the audio (RFC 4733)", "В звуке (RFC 4733)"),
        DtmfMode::Info => l.t("SIP INFO messages", "Сообщения SIP INFO"),
    }
}

fn transport_hint(transport: TransportKind, l: Lang) -> &'static str {
    match transport {
        TransportKind::Udp => l.t(
            "The usual choice: fast and works on most networks.",
            "Обычный выбор: быстро и работает в большинстве сетей.",
        ),
        TransportKind::Tcp => l.t(
            "Reliable. Use it when UDP is blocked or calls get cut off.",
            "Надёжно. Подойдёт, если UDP заблокирован или звонки обрываются.",
        ),
        TransportKind::Tls => l.t(
            "Encrypted signalling. The station needs a certificate.",
            "Шифрованная сигнализация. У станции должен быть сертификат.",
        ),
        TransportKind::Ws => l.t(
            "For stations that accept WebSocket connections.",
            "Для станций, которые принимают соединения WebSocket.",
        ),
        TransportKind::Wss => l.t(
            "Encrypted WebSocket. Trusts the system's certificates.",
            "Шифрованный WebSocket. Использует сертификаты системы.",
        ),
    }
}

impl PhoneApp {
    pub(super) fn open_settings(&mut self, tab: SettingsTab) {
        self.devices = audio::list_devices();
        self.settings = Some(SettingsDraft {
            tab,
            server: self.form.server.clone(),
            extension: self.form.extension.clone(),
            password: String::new(),
            connection: self.stored.connection.clone(),
            audio: self.stored.audio.clone(),
            calls: self.stored.calls.clone(),
            missing: false,
        });
    }

    /// Applies the edited settings: saves them, switches the audio devices and, if the account
    /// changed while signed in, reconnects.
    fn save_settings(&mut self) {
        let Some(draft) = self.settings.as_mut() else {
            return;
        };
        if draft.server.trim().is_empty() || draft.extension.trim().is_empty() {
            draft.missing = true;
            return;
        }
        let Some(draft) = self.settings.take() else {
            return;
        };

        let old_server = self.stored.server.clone();
        let old_extension = self.stored.extension.clone();
        let server = draft.server.trim().to_string();
        let extension = draft.extension.trim().to_string();
        let mut connection = draft.connection;
        connection.expiry_secs = connection.expiry();
        let account_changed = old_server != server
            || old_extension != extension
            || self.stored.connection != connection
            || !draft.password.is_empty();

        self.stored.server = server.clone();
        self.stored.extension = extension.clone();
        self.stored.connection = connection.clone();
        self.stored.audio = draft.audio.clone();
        self.stored.calls = draft.calls.clone();
        store::save(&self.stored);
        self.form.server = server.clone();
        self.form.extension = extension.clone();
        self.send(Command::SetAudio(draft.audio));
        self.send(Command::SetCalls(draft.calls));

        if self.signed_in && account_changed {
            let password = if draft.password.is_empty() {
                store::load_password(&old_server, &old_extension)
            } else {
                Some(draft.password.clone())
            };
            match password {
                Some(password) => {
                    let account = Account {
                        server,
                        extension,
                        password,
                        connection,
                    };
                    let keys_changed =
                        old_server != account.server || old_extension != account.extension;
                    if store::save_password(&account).is_err() {
                        self.show_toast(Notice::PasswordNotSaved);
                    }
                    if keys_changed {
                        store::forget_password(&old_server, &old_extension);
                    }
                    self.reg = RegState::Connecting;
                    self.send(Command::Register(account));
                }
                // The saved password is gone (another station or extension): ask for it again.
                None => {
                    self.send(Command::Unregister);
                    self.signed_in = false;
                    self.reg = RegState::Offline;
                }
            }
        } else if !draft.password.is_empty() {
            self.form.password = draft.password;
        }
    }

    pub(super) fn settings_screen(&mut self, ui: &mut Ui) {
        let l = self.lang();
        let Some(mut draft) = self.settings.take() else {
            return;
        };
        let mut close = false;
        let mut save = false;
        let mut sign_out = false;
        let mut language: Option<Lang> = None;

        // Title row: a back arrow and the title.
        let title_row = ui.allocate_space(vec2(ui.available_width(), 40.0)).1;
        let back = Rect::from_min_size(title_row.min, vec2(84.0, title_row.height()));
        let back_response = ui
            .interact(back, ui.id().with("settings_back"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        draw_chevron_left(
            ui,
            pos2(back.left() + 8.0, back.center().y),
            if back_response.hovered() { TEXT } else { MUTED },
        );
        ui.painter().text(
            pos2(back.left() + 22.0, back.center().y),
            Align2::LEFT_CENTER,
            l.t("Back", "Назад"),
            FontId::proportional(14.5),
            if back_response.hovered() { TEXT } else { MUTED },
        );
        ui.painter().text(
            title_row.center(),
            Align2::CENTER_CENTER,
            l.t("Settings", "Настройки"),
            FontId::proportional(17.0),
            TEXT,
        );
        if back_response.clicked() {
            close = true;
        }
        ui.add_space(8.0);

        // Tabs.
        let labels: Vec<&str> = SettingsTab::ALL.iter().map(|t| t.label(l)).collect();
        let current = SettingsTab::ALL
            .iter()
            .position(|t| *t == draft.tab)
            .unwrap_or(0);
        if let Some(chosen) = segmented(ui, &labels, current) {
            draft.tab = SettingsTab::ALL[chosen];
        }
        ui.add_space(14.0);

        // The tab's content scrolls; the Save button below it stays put.
        // A visible scroll bar tells the user there is more below (egui hides its default one
        // until the mouse is over it).
        ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
        let reserved = 74.0;
        egui::ScrollArea::vertical()
            .id_salt("settings_scroll")
            .auto_shrink([false, false])
            .max_height((ui.available_height() - reserved).max(120.0))
            .show(ui, |ui| match draft.tab {
                SettingsTab::Account => {
                    self.account_tab(ui, &mut draft, l, &mut sign_out);
                }
                SettingsTab::Network => self.network_tab(ui, &mut draft, l),
                SettingsTab::Audio => self.audio_tab(ui, &mut draft, l),
                SettingsTab::General => language = self.general_tab(ui, &mut draft, l),
            });

        ui.add_space(8.0);
        if draft.missing {
            ui.label(
                RichText::new(l.t(
                    "Fill in the station address and the extension number",
                    "Заполните адрес станции и внутренний номер",
                ))
                .size(12.5)
                .color(RED),
            );
        }
        if pill(
            ui,
            48.0,
            ACCENT,
            l.t("Save changes", "Сохранить"),
            Color32::WHITE,
            true,
        ) {
            save = true;
        }

        self.settings = Some(draft);
        if let Some(lang) = language {
            self.set_language(lang);
        }
        if sign_out {
            self.settings = None;
            self.sign_out();
        } else if save {
            self.save_settings();
        } else if close {
            self.settings = None;
        }
    }

    fn account_tab(&self, ui: &mut Ui, draft: &mut SettingsDraft, l: Lang, sign_out: &mut bool) {
        field(
            ui,
            l.t("Station address", "Адрес станции"),
            l.t(
                "name or IP, for example pbx.example.com",
                "имя или IP, например pbx.example.com",
            ),
            &mut draft.server,
            false,
        );
        field(
            ui,
            l.t("Extension number", "Внутренний номер"),
            l.t("for example, 300", "например, 300"),
            &mut draft.extension,
            false,
        );
        field(
            ui,
            l.t("Display name", "Имя для звонков"),
            l.t(
                "shown to the person you call",
                "увидит тот, кому вы звоните",
            ),
            &mut draft.connection.display_name,
            false,
        );
        let password_hint = if self.signed_in {
            l.t(
                "leave empty to keep the current one",
                "оставьте пустым, чтобы не менять",
            )
        } else {
            ""
        };
        field(
            ui,
            l.t("Password", "Пароль"),
            password_hint,
            &mut draft.password,
            true,
        );
        hint(
            ui,
            l.t(
                "Add :port to the address only if your station uses a non-standard one. Without a port the phone finds the station through its DNS records.",
                "Добавляйте :порт к адресу, только если станция использует нестандартный. Без порта телефон найдёт станцию по её DNS-записям.",
            ),
        );
        if self.signed_in {
            ui.add_space(8.0);
            if pill(
                ui,
                44.0,
                SURFACE_HI,
                l.t("Sign out", "Выйти из аккаунта"),
                RED,
                true,
            ) {
                *sign_out = true;
            }
        }
        ui.add_space(8.0);
    }

    fn network_tab(&self, ui: &mut Ui, draft: &mut SettingsDraft, l: Lang) {
        let connection = &mut draft.connection;

        label(ui, l.t("Connection type", "Тип подключения"));
        let options: Vec<String> = TransportKind::ALL
            .iter()
            .map(|t| transport_label(*t, l))
            .collect();
        let selected = TransportKind::ALL
            .iter()
            .position(|t| *t == connection.transport)
            .unwrap_or(0);
        if let Some(chosen) = dropdown(ui, "transport", &options, selected) {
            connection.transport = TransportKind::ALL[chosen];
        }
        hint(ui, transport_hint(connection.transport, l));
        ui.add_space(10.0);

        field(
            ui,
            l.t("SIP domain", "SIP-домен"),
            l.t(
                "only if it differs from the station",
                "только если отличается от станции",
            ),
            &mut connection.domain,
            false,
        );
        field(
            ui,
            l.t("Outbound proxy", "Прокси для исходящих"),
            l.t(
                "optional, e.g. proxy.example.com:5060",
                "необязательно, например proxy.example.com:5060",
            ),
            &mut connection.outbound_proxy,
            false,
        );

        label(ui, l.t("Keep the registration for", "Срок регистрации"));
        let mut presets: Vec<u32> = EXPIRY_PRESETS.to_vec();
        let expiry = connection
            .expiry_secs
            .clamp(MIN_EXPIRY_SECS, MAX_EXPIRY_SECS);
        if !presets.contains(&expiry) {
            presets.push(expiry);
            presets.sort_unstable();
        }
        let options: Vec<String> = presets.iter().map(|s| expiry_label(*s, l)).collect();
        let selected = presets.iter().position(|s| *s == expiry).unwrap_or(1);
        if let Some(chosen) = dropdown(ui, "expiry", &options, selected) {
            connection.expiry_secs = presets[chosen];
        }
        hint(
            ui,
            l.t(
                "How long the station remembers the phone. It is renewed automatically.",
                "Сколько станция помнит телефон. Продлевается автоматически.",
            ),
        );
        ui.add_space(10.0);

        if matches!(connection.transport, TransportKind::Ws | TransportKind::Wss) {
            field(
                ui,
                l.t("WebSocket path", "Путь WebSocket"),
                "/",
                &mut connection.ws_path,
                false,
            );
        }
        if connection.transport == TransportKind::Tls {
            field(
                ui,
                l.t("Extra certificate file", "Дополнительный сертификат"),
                l.t(
                    "optional, for a private certificate (PEM)",
                    "необязательно, для собственного сертификата (PEM)",
                ),
                &mut connection.ca_path,
                false,
            );
        }

        toggle_row(
            ui,
            l.t("Advertise the public address", "Сообщать внешний адрес"),
            l.t(
                "Helps when you are behind a router (NAT). Turn it off if audio breaks.",
                "Помогает, если вы за роутером (NAT). Выключите, если пропал звук.",
            ),
            &mut connection.use_public_address,
        );
        hint(
            ui,
            l.t(
                "Changes apply when you press Save: the phone reconnects.",
                "Изменения применятся после сохранения: телефон переподключится.",
            ),
        );
        ui.add_space(8.0);
    }

    fn audio_tab(&self, ui: &mut Ui, draft: &mut SettingsDraft, l: Lang) {
        let devices: &AudioDevices = &self.devices;
        let default_label = |default: &Option<String>| match default {
            Some(name) => format!("{} ({name})", l.t("System default", "По умолчанию")),
            None => l.t("System default", "По умолчанию").to_string(),
        };

        label(ui, l.t("Microphone", "Микрофон"));
        let mut options = vec![default_label(&devices.default_input)];
        options.extend(devices.inputs.iter().cloned());
        let selected = draft
            .audio
            .input_device
            .as_ref()
            .and_then(|name| devices.inputs.iter().position(|d| d == name))
            .map_or(0, |i| i + 1);
        if let Some(chosen) = dropdown(ui, "microphone", &options, selected) {
            draft.audio.input_device = (chosen > 0).then(|| devices.inputs[chosen - 1].clone());
        }
        ui.add_space(14.0);

        label(ui, l.t("Speaker", "Динамик"));
        let mut options = vec![default_label(&devices.default_output)];
        options.extend(devices.outputs.iter().cloned());
        let selected = draft
            .audio
            .output_device
            .as_ref()
            .and_then(|name| devices.outputs.iter().position(|d| d == name))
            .map_or(0, |i| i + 1);
        if let Some(chosen) = dropdown(ui, "speaker", &options, selected) {
            draft.audio.output_device = (chosen > 0).then(|| devices.outputs[chosen - 1].clone());
        }
        hint(
            ui,
            l.t(
                "The ringtone plays on the chosen speaker. Devices are used from the next call.",
                "Мелодия звонка играет на выбранном динамике. Устройства применяются со следующего звонка.",
            ),
        );
        ui.add_space(8.0);
    }

    /// Returns the language if the user changed it.
    fn general_tab(&self, ui: &mut Ui, draft: &mut SettingsDraft, l: Lang) -> Option<Lang> {
        let mut chosen_language = None;
        label(ui, l.t("Language", "Язык"));
        let options: Vec<String> = Lang::ALL
            .iter()
            .map(|lang| lang.name().to_string())
            .collect();
        let selected = Lang::ALL.iter().position(|lang| *lang == l).unwrap_or(0);
        if let Some(chosen) = dropdown(ui, "language_setting", &options, selected) {
            chosen_language = Some(Lang::ALL[chosen]);
        }
        ui.add_space(18.0);

        label(ui, l.t("Calls", "Звонки"));
        toggle_row(
            ui,
            l.t("Do not disturb", "Не беспокоить"),
            l.t(
                "Incoming calls are turned away and show up as missed.",
                "Входящие отклоняются и попадают в пропущенные.",
            ),
            &mut draft.calls.do_not_disturb,
        );
        toggle_row(
            ui,
            l.t("Answer automatically", "Отвечать автоматически"),
            l.t(
                "Picks up after a moment. The microphone stays off until you switch it on.",
                "Снимает трубку через мгновение. Микрофон остаётся выключенным, пока вы его не включите.",
            ),
            &mut draft.calls.auto_answer,
        );
        ui.add_space(4.0);
        label(ui, l.t("Keypad tones", "Тоны клавиш"));
        let options: Vec<String> = DtmfMode::ALL
            .iter()
            .map(|m| dtmf_label(*m, l).into())
            .collect();
        let selected = DtmfMode::ALL
            .iter()
            .position(|m| *m == draft.calls.dtmf_mode)
            .unwrap_or(0);
        if let Some(chosen) = dropdown(ui, "dtmf", &options, selected) {
            draft.calls.dtmf_mode = DtmfMode::ALL[chosen];
        }
        hint(
            ui,
            l.t(
                "How keys pressed during a call reach voice menus. Automatic uses what the station supports.",
                "Как нажатия клавиш во время звонка доходят до голосового меню. Автоматический режим использует то, что поддерживает станция.",
            ),
        );
        ui.add_space(18.0);

        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(CornerRadius::same(14))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(format!("RustSIPPhone {}", env!("CARGO_PKG_VERSION")))
                        .size(15.0)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(l.t(
                        "A free, open-source desktop SIP phone. MIT license.",
                        "Бесплатный телефон с открытым кодом для SIP. Лицензия MIT.",
                    ))
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(10.0);
                link(ui, "GitHub: DjTim0n/RustSIPPhone", REPOSITORY_URL);
            });
        ui.add_space(8.0);
        chosen_language
    }
}

// ------------------------------------------------------------------ widgets

fn label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(13.0).color(MUTED));
    ui.add_space(4.0);
}

fn hint(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(
        RichText::new(text)
            .size(12.0)
            .color(MUTED.gamma_multiply(0.85)),
    );
}

/// A link in the accent colour that opens `url` in the browser, underlined while the mouse is on it.
fn link(ui: &mut Ui, text: &str, url: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(14.0), ACCENT);
    let size = galley.size();
    let rect = ui.allocate_space(vec2(size.x, size.y + 4.0)).1;
    let response = ui
        .interact(rect, ui.id().with(("link", url)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(url);
    ui.painter().galley(rect.min, galley, ACCENT);
    if response.hovered() {
        let y = rect.min.y + size.y + 1.0;
        ui.painter().line_segment(
            [pos2(rect.min.x, y), pos2(rect.min.x + size.x, y)],
            Stroke::new(1.0, ACCENT),
        );
    }
    if response.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

fn draw_chevron_left(ui: &Ui, center: Pos2, color: Color32) {
    let stroke = Stroke::new(1.8, color);
    ui.painter().line_segment(
        [
            pos2(center.x + 3.0, center.y - 5.0),
            pos2(center.x - 2.0, center.y),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            pos2(center.x - 2.0, center.y),
            pos2(center.x + 3.0, center.y + 5.0),
        ],
        stroke,
    );
}

/// Draws a small gear: a ring with eight teeth.
pub(super) fn draw_gear(ui: &Ui, center: Pos2, radius: f32, color: Color32) {
    let painter = ui.painter();
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.0;
        let (sin, cos) = angle.sin_cos();
        painter.line_segment(
            [
                pos2(
                    center.x + cos * radius * 0.62,
                    center.y + sin * radius * 0.62,
                ),
                pos2(center.x + cos * radius, center.y + sin * radius),
            ],
            Stroke::new(radius * 0.32, color),
        );
    }
    painter.circle_stroke(center, radius * 0.62, Stroke::new(radius * 0.26, color));
}

/// A row of equally wide choices. Returns the index picked this frame.
fn segmented(ui: &mut Ui, labels: &[&str], selected: usize) -> Option<usize> {
    let rect = ui.allocate_space(vec2(ui.available_width(), 38.0)).1;
    ui.painter()
        .rect_filled(rect, CornerRadius::same(19), SURFACE);
    let width = rect.width() / labels.len() as f32;
    let mut picked = None;
    for (i, text) in labels.iter().enumerate() {
        let cell = Rect::from_min_size(
            pos2(rect.left() + width * i as f32, rect.top()),
            vec2(width, rect.height()),
        )
        .shrink(3.0);
        let response = ui
            .interact(cell, ui.id().with(("segment", i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let active = i == selected;
        if active {
            ui.painter()
                .rect_filled(cell, CornerRadius::same(16), SURFACE_HI);
        }
        ui.painter().text(
            cell.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(13.5),
            if active { TEXT } else { MUTED },
        );
        if response.clicked() {
            picked = Some(i);
        }
    }
    picked
}

/// A full-width select, drawn like the text fields. Returns the index chosen this frame.
fn dropdown(ui: &mut Ui, id: &str, options: &[String], selected: usize) -> Option<usize> {
    let rect = ui.allocate_space(vec2(ui.available_width(), 44.0)).1;
    let button = ui
        .interact(rect, ui.id().with(("dropdown", id)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&button));

    let radius = CornerRadius::same(12);
    ui.painter().rect_filled(rect, radius, SURFACE);
    ui.painter().rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, if open { ACCENT } else { SURFACE_HI }),
        egui::StrokeKind::Inside,
    );
    // Long names are cut at the arrow instead of running over it.
    let text_area = Rect::from_min_max(rect.min, pos2(rect.right() - 34.0, rect.bottom()));
    ui.painter().with_clip_rect(text_area).text(
        pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        options.get(selected).map_or("", String::as_str),
        FontId::proportional(15.0),
        TEXT,
    );
    draw_chevron(ui, pos2(rect.right() - 20.0, rect.center().y), open);

    let mut picked = None;
    let frame = egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, SURFACE_HI))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(4))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(120),
        });
    egui::Popup::from_toggle_button_response(&button)
        .at_position(pos2(rect.left(), rect.bottom() + 6.0))
        .gap(0.0)
        .width(rect.width())
        .frame(frame)
        .show(|ui| {
            ui.set_width(rect.width() - 8.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, text) in options.iter().enumerate() {
                        if option_row(ui, text, i == selected) {
                            picked = Some(i);
                        }
                    }
                });
        });
    picked.filter(|i| *i != selected)
}

fn option_row(ui: &mut Ui, text: &str, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let radius = CornerRadius::same(10);
    if response.hovered() {
        ui.painter().rect_filled(rect, radius, SURFACE_HI);
    } else if selected {
        ui.painter()
            .rect_filled(rect, radius, ACCENT.gamma_multiply(0.14));
    }
    ui.painter()
        .with_clip_rect(rect.shrink2(vec2(6.0, 0.0)))
        .text(
            pos2(rect.left() + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(14.5),
            if selected { ACCENT } else { TEXT },
        );
    response.clicked()
}

/// A titled on/off switch with a short explanation underneath.
fn toggle_row(ui: &mut Ui, title: &str, description: &str, value: &mut bool) {
    let width = ui.available_width();
    let text_width = width - 64.0;
    let galley = ui.painter().layout(
        description.to_string(),
        FontId::proportional(12.0),
        MUTED.gamma_multiply(0.85),
        text_width,
    );
    let height = 26.0 + galley.size().y + 12.0;
    let rect = ui.allocate_space(vec2(width, height)).1;
    let response = ui
        .interact(rect, ui.id().with(("toggle", title)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().text(
        pos2(rect.left(), rect.top() + 12.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(14.5),
        TEXT,
    );
    ui.painter()
        .galley(pos2(rect.left(), rect.top() + 26.0), galley, MUTED);

    // The switch.
    let switch = Rect::from_center_size(
        pos2(rect.right() - 22.0, rect.top() + 14.0),
        vec2(44.0, 26.0),
    );
    let track = if *value { GREEN } else { SURFACE_HI };
    ui.painter()
        .rect_filled(switch, CornerRadius::same(13), track);
    let knob_x = if *value {
        switch.right() - 13.0
    } else {
        switch.left() + 13.0
    };
    ui.painter()
        .circle_filled(pos2(knob_x, switch.center().y), 10.0, Color32::WHITE);
    if response.clicked() {
        *value = !*value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app on a throwaway settings directory, so the tests never touch the real settings.
    fn test_app() -> PhoneApp {
        static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        let dir = DIR.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("rsp-ui-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            // SAFETY: set once, before any test thread reads it, and never changed afterwards.
            unsafe { std::env::set_var("RUSTSIPPHONE_CONFIG_DIR", &dir) };
            dir.clone()
        });
        let _ = dir;
        PhoneApp::build(&egui::Context::default())
    }

    #[test]
    fn saving_applies_the_edited_settings() {
        let mut app = test_app();
        app.open_settings(SettingsTab::Network);
        {
            let draft = app.settings.as_mut().unwrap();
            draft.server = " pbx.example.com ".into();
            draft.extension = "300".into();
            draft.connection.transport = TransportKind::Tls;
            draft.connection.domain = "example.com".into();
            draft.connection.display_name = "Tim".into();
            draft.audio.output_device = Some("Headphones".into());
        }
        app.save_settings();
        assert!(app.settings.is_none(), "the screen closes after saving");
        assert_eq!(
            app.stored.server, "pbx.example.com",
            "the address is trimmed"
        );
        assert_eq!(app.stored.connection.transport, TransportKind::Tls);
        assert_eq!(app.stored.connection.domain, "example.com");
        assert_eq!(app.stored.connection.display_name, "Tim");
        assert_eq!(
            app.stored.audio.output_device.as_deref(),
            Some("Headphones")
        );
        // The sign-in form now shows what was saved.
        assert_eq!(app.form.server, "pbx.example.com");
    }

    #[test]
    fn saving_without_an_address_is_refused() {
        let mut app = test_app();
        app.open_settings(SettingsTab::Account);
        let draft = app.settings.as_mut().unwrap();
        draft.server.clear();
        draft.extension = "300".into();
        app.save_settings();
        let draft = app.settings.as_ref().expect("still open");
        assert!(draft.missing);
    }

    #[test]
    fn expiry_is_clamped_when_saving() {
        let mut app = test_app();
        app.open_settings(SettingsTab::Network);
        let draft = app.settings.as_mut().unwrap();
        draft.server = "pbx.example.com".into();
        draft.extension = "300".into();
        draft.connection.expiry_secs = 5;
        app.save_settings();
        assert_eq!(app.stored.connection.expiry_secs, MIN_EXPIRY_SECS);
    }

    /// Runs `body` in a headless egui frame, with `events` as the input.
    fn frame(
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        mut body: impl FnMut(&mut Ui),
    ) {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 720.0))),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| body(ui));
        output.textures_delta.clear();
    }

    fn click(ctx: &egui::Context, time: &mut f64, at: Pos2, mut body: impl FnMut(&mut Ui)) {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        for events in [
            vec![egui::Event::PointerMoved(at)],
            vec![button(true)],
            vec![button(false)],
        ] {
            *time += 0.05;
            frame(ctx, *time, events, &mut body);
        }
    }

    #[test]
    fn dropdown_opens_and_picks_an_option() {
        let ctx = egui::Context::default();
        let options: Vec<String> = ["UDP", "TCP", "TLS"].map(String::from).to_vec();
        let mut time = 0.0;
        let mut picked = None;
        let mut button_rect = Rect::NOTHING;

        // First frame: find out where the dropdown is drawn.
        frame(&ctx, time, vec![], |ui| {
            let top_left = ui.available_rect_before_wrap().min;
            button_rect = Rect::from_min_size(top_left, vec2(ui.available_width(), 44.0));
            dropdown(ui, "test", &options, 0);
        });

        click(&ctx, &mut time, button_rect.center(), |ui| {
            dropdown(ui, "test", &options, 0);
        });
        for _ in 0..3 {
            time += 0.05;
            frame(&ctx, time, vec![], |ui| {
                dropdown(ui, "test", &options, 0);
            });
        }
        assert!(egui::Popup::is_any_open(&ctx), "the list opens on a click");

        // Rows sit under the field: popup margin 4, rows 38 high with 2 between them.
        let third_row = pos2(
            button_rect.center().x,
            button_rect.bottom() + 6.0 + 4.0 + 2.0 * (38.0 + 2.0) + 19.0,
        );
        click(&ctx, &mut time, third_row, |ui| {
            if let Some(i) = dropdown(ui, "test", &options, 0) {
                picked = Some(i);
            }
        });
        assert_eq!(picked, Some(2), "the third option, TLS, was chosen");
    }

    #[test]
    fn toggle_flips_on_click() {
        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut value = true;
        let mut switch_center = Pos2::ZERO;
        frame(&ctx, time, vec![], |ui| {
            let top_left = ui.available_rect_before_wrap().min;
            switch_center = pos2(top_left.x + ui.available_width() - 22.0, top_left.y + 14.0);
            toggle_row(ui, "Public address", "Helps behind NAT.", &mut value);
        });
        click(&ctx, &mut time, switch_center, |ui| {
            toggle_row(ui, "Public address", "Helps behind NAT.", &mut value);
        });
        assert!(!value, "one click switches it off");
    }

    /// Draws the main screens into PNG files so they can be looked at. It renders on the GPU and
    /// writes files, so it is run by hand:
    /// `RSP_RENDER_DIR=/some/dir cargo test render_screens -- --ignored --nocapture`
    #[test]
    #[ignore = "renders on the GPU and writes files; run by hand"]
    fn render_screens() {
        use crate::audio::AudioDevices;

        let out = std::env::var_os("RSP_RENDER_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("rsp-screens"));
        std::fs::create_dir_all(&out).unwrap();

        type Prepare = Box<dyn Fn(&mut PhoneApp)>;
        let online: Prepare = Box::new(|app| {
            app.signed_in = true;
            app.reg = RegState::Online;
            app.stored.extension = "888".into();
            app.form.server = "pbx.example.com".into();
            app.form.extension = "888".into();
        });
        let devices = AudioDevices {
            inputs: vec!["MacBook Pro Microphone".into(), "USB Headset".into()],
            outputs: vec!["MacBook Pro Speakers".into(), "USB Headset".into()],
            default_input: Some("MacBook Pro Microphone".into()),
            default_output: Some("MacBook Pro Speakers".into()),
        };
        let screens: Vec<(&str, Prepare)> = vec![
            (
                "login",
                Box::new(|app| {
                    app.form.server = "pbx.example.com".into();
                    app.form.extension = "888".into();
                }),
            ),
            (
                "settings-account",
                Box::new({
                    let devices = devices.clone();
                    move |app| {
                        app.signed_in = true;
                        app.reg = RegState::Online;
                        app.form.server = "pbx.example.com".into();
                        app.form.extension = "888".into();
                        app.open_settings(SettingsTab::Account);
                        app.devices = devices.clone();
                    }
                }),
            ),
            (
                "settings-network-tls",
                Box::new(|app| {
                    app.form.server = "pbx.example.com".into();
                    app.form.extension = "888".into();
                    app.open_settings(SettingsTab::Network);
                    if let Some(draft) = app.settings.as_mut() {
                        draft.connection.transport = TransportKind::Tls;
                        draft.connection.domain = "example.com".into();
                    }
                }),
            ),
            (
                "settings-network-ws",
                Box::new(|app| {
                    app.open_settings(SettingsTab::Network);
                    if let Some(draft) = app.settings.as_mut() {
                        draft.connection.transport = TransportKind::Wss;
                    }
                }),
            ),
            (
                "settings-audio",
                Box::new({
                    let devices = devices.clone();
                    move |app| {
                        app.open_settings(SettingsTab::Audio);
                        app.devices = devices.clone();
                        if let Some(draft) = app.settings.as_mut() {
                            draft.audio.input_device = Some("USB Headset".into());
                        }
                    }
                }),
            ),
            (
                "settings-general",
                Box::new(|app| {
                    app.open_settings(SettingsTab::General);
                }),
            ),
            ("phone", online),
        ];

        let mut screens = screens;
        let active = |app: &mut PhoneApp, tweak: &dyn Fn(&mut CallView)| {
            app.signed_in = true;
            app.reg = RegState::Online;
            let mut view = CallView {
                connected_at: Some(Instant::now() - Duration::from_secs(42)),
                ..CallView::new("7777", Phase::Active)
            };
            tweak(&mut view);
            app.call = Some(view);
        };
        screens.push(("call-active", Box::new(move |app| active(app, &|_| {}))));
        screens.push((
            "call-keypad",
            Box::new(move |app| {
                active(app, &|_| {});
                app.keypad_open = true;
            }),
        ));
        screens.push((
            "call-hold",
            Box::new(move |app| active(app, &|v| v.local_hold = true)),
        ));
        screens.push((
            "call-remote-hold",
            Box::new(move |app| active(app, &|v| v.remote_hold = true)),
        ));
        screens.push((
            "call-transfer",
            Box::new(move |app| {
                active(app, &|_| {});
                app.transfer_open = true;
            }),
        ));
        screens.push((
            "call-transferring",
            Box::new(move |app| active(app, &|v| v.transferring = true)),
        ));
        screens.push((
            "phone-dnd",
            Box::new(|app| {
                app.signed_in = true;
                app.reg = RegState::Online;
                app.stored.extension = "888".into();
                app.stored.calls.do_not_disturb = true;
            }),
        ));
        screens.push((
            "settings-network-tall",
            Box::new(|app| {
                app.open_settings(SettingsTab::Network);
                if let Some(draft) = app.settings.as_mut() {
                    draft.connection.transport = TransportKind::Wss;
                }
            }),
        ));
        for (name, prepare) in screens {
            let height = if name.ends_with("tall") {
                1100.0
            } else {
                720.0
            };
            let mut app = test_app();
            prepare(&mut app);
            let mut harness = egui_kittest::Harness::builder()
                .with_size(vec2(400.0, height))
                .with_pixels_per_point(2.0)
                .build_ui(|ui| app.show(ui));
            apply_style(&harness.ctx);
            harness.run();
            let image = harness.render().expect("GPU rendering is available");
            let path = out.join(format!("{name}.png"));
            image.save(&path).expect("the PNG can be written");
            println!("wrote {}", path.display());
        }
    }

    #[test]
    fn the_github_link_opens_the_repository() {
        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut centre = Pos2::ZERO;
        frame(&ctx, time, vec![], |ui| {
            let top_left = ui.available_rect_before_wrap().min;
            link(ui, "GitHub: DjTim0n/RustSIPPhone", REPOSITORY_URL);
            centre = top_left + vec2(40.0, 9.0);
        });
        let at = centre;
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let mut opened = Vec::new();
        for events in [
            vec![egui::Event::PointerMoved(at)],
            vec![button(true)],
            vec![button(false)],
        ] {
            time += 0.05;
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 720.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                link(ui, "GitHub: DjTim0n/RustSIPPhone", REPOSITORY_URL)
            });
            output.textures_delta.clear();
            for command in &output.platform_output.commands {
                if let egui::OutputCommand::OpenUrl(open) = command {
                    opened.push(open.url.clone());
                }
            }
        }
        assert_eq!(opened, vec![REPOSITORY_URL.to_string()]);
    }

    #[test]
    fn expiry_labels_are_localized() {
        assert_eq!(expiry_label(60, Lang::English), "1 minute");
        assert_eq!(expiry_label(60, Lang::Russian), "1 минута");
        assert_eq!(expiry_label(45, Lang::English), "45 s");
    }

    #[test]
    fn every_transport_has_a_label_and_a_hint_in_both_languages() {
        for transport in TransportKind::ALL {
            for lang in Lang::ALL {
                assert!(!transport_label(transport, lang).is_empty());
                assert!(!transport_hint(transport, lang).is_empty());
            }
        }
    }
}
