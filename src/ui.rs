//! The phone's interface, built with egui.

use crate::audio::AudioDevices;
use crate::i18n::Lang;
use crate::model::*;
use crate::store::{self, Stored};
use crate::tray::{Tray, TrayAction};
use settings_screen::{SettingsDraft, SettingsTab};

mod settings_screen;
use crate::window;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Pos2, Rect, RichText, Sense, Stroke,
    TextEdit, Ui, UiBuilder, Vec2, pos2, vec2,
};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

// Palette: dark, one blue accent; green and red are reserved for answering and hanging up.
const BG: Color32 = Color32::from_rgb(0x12, 0x15, 0x1A);
const SURFACE: Color32 = Color32::from_rgb(0x1B, 0x20, 0x27);
const SURFACE_HI: Color32 = Color32::from_rgb(0x26, 0x2D, 0x37);
const TEXT: Color32 = Color32::from_rgb(0xEC, 0xEF, 0xF4);
const MUTED: Color32 = Color32::from_rgb(0x8C, 0x96, 0xA5);
const ACCENT: Color32 = Color32::from_rgb(0x4C, 0x8D, 0xFF);
const GREEN: Color32 = Color32::from_rgb(0x2F, 0xB3, 0x5A);
const RED: Color32 = Color32::from_rgb(0xE5, 0x48, 0x4D);
const AMBER: Color32 = Color32::from_rgb(0xE0, 0xA0, 0x30);

const COLUMN_WIDTH: f32 = 340.0;
const DIAL_KEYS: [&str; 12] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "*", "0", "#"];
const TOAST_LIFETIME: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dial,
    Recent,
}

#[derive(Default)]
struct LoginForm {
    server: String,
    extension: String,
    password: String,
    error: Option<Notice>,
}

pub struct PhoneApp {
    // The runtime lives as long as the app: the core runs on it.
    runtime: tokio::runtime::Runtime,
    engine: Option<tokio::task::JoinHandle<()>>,
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,

    stored: Stored,
    /// We want to be online: show the phone, not the sign-in form.
    signed_in: bool,
    reg: RegState,
    form: LoginForm,

    tab: Tab,
    number: String,
    call: Option<CallView>,
    keypad_open: bool,
    /// The transfer panel of the call screen is open, with the target typed so far.
    transfer_open: bool,
    transfer_target: String,
    toast: Option<(Notice, Instant)>,
    /// The user chose to quit, so the next close request really closes the window.
    quitting: bool,
    /// The window was sent to the background and has not been brought back yet.
    in_background: bool,
    /// System tray icon (Windows). `None` where there is no tray, or if creating it failed.
    tray: Option<Tray>,
    /// The settings being edited; `Some` while the settings screen is open.
    settings: Option<SettingsDraft>,
    devices: AudioDevices,
}

impl PhoneApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_style(&cc.egui_ctx);
        Self::build(&cc.egui_ctx)
    }

    /// Creates the app and starts the phone core. Separate from `new` so tests can build one
    /// without a window.
    fn build(egui_ctx: &egui::Context) -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("could not start the async runtime");
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let repaint_ctx = egui_ctx.clone();
        let events = Events::new(
            event_tx,
            std::sync::Arc::new(move || repaint_ctx.request_repaint()),
        );
        let engine = runtime.spawn(crate::engine::run(command_rx, events));

        let stored = store::load();
        let tray = Tray::new(egui_ctx, stored.language);
        let mut app = PhoneApp {
            runtime,
            engine: Some(engine),
            commands: command_tx,
            events: event_rx,
            form: LoginForm {
                server: stored.server.clone(),
                extension: stored.extension.clone(),
                ..Default::default()
            },
            stored,
            signed_in: false,
            reg: RegState::Offline,
            tab: Tab::Dial,
            number: String::new(),
            call: None,
            keypad_open: false,
            transfer_open: false,
            transfer_target: String::new(),
            toast: None,
            quitting: false,
            in_background: false,
            tray,
            settings: None,
            devices: AudioDevices::default(),
        };
        app.send(Command::SetAudio(app.stored.audio.clone()));
        app.send(Command::SetCalls(app.stored.calls.clone()));
        app.try_auto_login();
        app
    }

    /// If the account is already stored together with its password, sign in without asking.
    fn try_auto_login(&mut self) {
        if self.stored.server.is_empty() || self.stored.extension.is_empty() {
            return;
        }
        if let Some(password) = store::load_password(&self.stored.server, &self.stored.extension) {
            let account = Account {
                server: self.stored.server.clone(),
                extension: self.stored.extension.clone(),
                password,
                connection: self.stored.connection.clone(),
            };
            self.signed_in = true;
            self.reg = RegState::Connecting;
            self.send(Command::Register(account));
        }
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    fn submit_login(&mut self) {
        // The address is used as typed. Without a port the stack looks up the station's SRV
        // records and falls back to the transport's default port.
        let account = Account {
            server: self.form.server.trim().to_string(),
            extension: self.form.extension.trim().to_string(),
            password: self.form.password.clone(),
            connection: self.stored.connection.clone(),
        };
        // No password store (for example Linux without gnome-keyring) is no reason to refuse:
        // sign in, but honestly warn that the password will have to be typed again.
        let password_saved = store::save_password(&account).is_ok();
        self.stored.server = account.server.clone();
        self.stored.extension = account.extension.clone();
        store::save(&self.stored);
        self.form.error = None;
        self.signed_in = true;
        self.reg = RegState::Connecting;
        self.send(Command::Register(account));
        if !password_saved {
            self.show_toast(Notice::PasswordNotSaved);
        }
    }

    fn sign_out(&mut self) {
        self.send(Command::Unregister);
        store::forget_password(&self.stored.server, &self.stored.extension);
        self.signed_in = false;
        self.reg = RegState::Offline;
        self.form.password.clear();
        self.form.error = None;
    }

    fn start_call(&mut self, number: &str) {
        let number = number.trim();
        if number.is_empty() {
            return;
        }
        self.send(Command::Dial(number.to_string()));
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Reg(state) => {
                    if let RegState::Failed {
                        notice,
                        retry: false,
                    } = &state
                    {
                        // The station rejected the details: take the user back to the form.
                        self.signed_in = false;
                        self.form.error = Some(notice.clone());
                    }
                    self.reg = state;
                }
                Event::Call(view) => {
                    // A call that starts ringing must surface even if the window is minimized or
                    // closed to the background; once it stops ringing the window goes back to normal.
                    let was_ringing = matches!(&self.call, Some(c) if c.phase == Phase::Incoming);
                    let is_ringing = matches!(&view, Some(c) if c.phase == Phase::Incoming);
                    if is_ringing && !was_ringing {
                        for command in window::incoming_call_commands() {
                            ctx.send_viewport_cmd(command);
                        }
                    } else if was_ringing && !is_ringing {
                        for command in window::ring_finished_commands() {
                            ctx.send_viewport_cmd(command);
                        }
                    }
                    if view.is_none() || self.call.is_none() {
                        self.keypad_open = false;
                        self.transfer_open = false;
                        self.transfer_target.clear();
                    }
                    self.call = view;
                }
                Event::CallEnded { entry, notice } => {
                    store::push_history(&mut self.stored, entry);
                    store::save(&self.stored);
                    if let Some(notice) = notice {
                        self.show_toast(notice);
                    }
                }
                Event::Toast(notice) => self.show_toast(notice),
            }
        }
    }

    fn show_toast(&mut self, notice: Notice) {
        self.toast = Some((notice, Instant::now()));
    }

    fn lang(&self) -> Lang {
        self.stored.language
    }

    /// A small "Quit app" link at the bottom. Closing the window only minimizes it (the phone
    /// must keep running to ring), so this is how the user really quits on every platform.
    fn quit_footer(&mut self, ui: &mut Ui) {
        let l = self.lang();
        let area = ui.max_rect();
        let rect = Rect::from_center_size(
            pos2(area.center().x, area.bottom() - 14.0),
            vec2(200.0, 28.0),
        );
        let response = ui
            .interact(rect, ui.id().with("quit_app"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            l.t("Quit app", "Закрыть приложение"),
            FontId::proportional(13.0),
            if response.hovered() { TEXT } else { MUTED },
        );
        if response.clicked() {
            self.quitting = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn set_language(&mut self, lang: Lang) {
        if self.stored.language != lang {
            self.stored.language = lang;
            store::save(&self.stored);
            if let Some(tray) = &self.tray {
                tray.set_language(lang);
            }
        }
    }
}

impl eframe::App for PhoneApp {
    /// Runs before every frame, and also while the window is minimized or hidden, when no frame is
    /// drawn at all. That is why events from the phone core are handled here and not in `ui`:
    /// an incoming call has to bring the window back even when nothing is on screen.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        // Clicks on the tray icon and its menu.
        let tray_actions = self.tray.as_ref().map(Tray::actions).unwrap_or_default();
        for action in tray_actions {
            match action {
                TrayAction::Show => {
                    for command in window::show_window_commands() {
                        ctx.send_viewport_cmd(command);
                    }
                }
                TrayAction::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            for command in window::close_request_commands(self.quitting, self.tray.is_some()) {
                ctx.send_viewport_cmd(command);
            }
            window::hide_application();
            self.in_background = true;
        }
        // The window is back after being in the background (Dock click, taskbar, a call):
        // make sure it is the active window and gets redrawn at once.
        if self.in_background
            && ctx.input(|i| {
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::WindowFocused(true)))
            })
        {
            self.in_background = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // During a call, digits typed on the keyboard go to the other side as tones.
        if self.call.as_ref().is_some_and(|c| c.phase == Phase::Active) {
            let typed: Vec<char> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) => Some(t.chars().collect::<Vec<_>>()),
                        _ => None,
                    })
                    .flatten()
                    .filter(|c| "0123456789*#".contains(*c))
                    .collect()
            });
            for digit in typed {
                self.send(Command::Dtmf(digit));
            }
            ctx.request_repaint_after(Duration::from_millis(500)); // call timer
        }
        if let Some((_, since)) = &self.toast {
            if since.elapsed() > TOAST_LIFETIME {
                self.toast = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(500));
            }
        }

        self.show(ui);
    }

    fn on_exit(&mut self) {
        // Ask the core to hang up and unregister, and wait for it briefly.
        self.send(Command::Shutdown);
        if let Some(engine) = self.engine.take() {
            // `timeout` creates a timer, which needs the runtime to be running already, so it
            // must be built inside the future and not as the argument of `block_on`.
            let _ = self
                .runtime
                .block_on(async { tokio::time::timeout(Duration::from_secs(8), engine).await });
        }
    }
}

impl PhoneApp {
    /// Draws the current screen. Kept apart from `ui` so tests can draw the app without a window.
    fn show(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ui, |ui| {
                let full = ui.available_rect_before_wrap();
                let width = full.width().min(COLUMN_WIDTH);
                let column = Rect::from_min_size(
                    pos2(full.center().x - width / 2.0, full.top()),
                    vec2(width, full.height()),
                );
                ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
                    if self.call.is_some() {
                        self.call_screen(ui);
                    } else if self.settings.is_some() {
                        self.settings_screen(ui);
                    } else if !self.signed_in {
                        self.login_screen(ui);
                    } else {
                        self.phone_screen(ui);
                    }
                });
            });
    }
}

// ------------------------------------------------------------------ sign in

impl PhoneApp {
    fn login_screen(&mut self, ui: &mut Ui) {
        let l = self.lang();
        let switch_row = ui.allocate_space(vec2(ui.available_width(), 32.0)).1;
        let switch_rect = Rect::from_min_size(
            pos2(switch_row.right() - LANG_SELECT_WIDTH, switch_row.top()),
            vec2(LANG_SELECT_WIDTH, switch_row.height()),
        );
        if let Some(chosen) = language_select(ui, switch_rect, l) {
            self.set_language(chosen);
        }
        let l = self.lang();

        ui.add_space(24.0);
        ui.label(
            RichText::new(l.t("Sign in to your phone", "Вход в телефон"))
                .size(26.0)
                .strong()
                .color(TEXT),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(l.t(
                "Enter the details your administrator gave you",
                "Введите данные, которые выдал администратор",
            ))
            .size(14.0)
            .color(MUTED),
        );
        ui.add_space(28.0);

        let connecting = matches!(self.reg, RegState::Connecting) && self.signed_in;
        field(
            ui,
            l.t("Station address", "Адрес станции"),
            l.t(
                "for example, 192.168.1.10:5060",
                "например, 192.168.1.10:5060",
            ),
            &mut self.form.server,
            false,
        );
        field(
            ui,
            l.t("Extension number", "Внутренний номер"),
            l.t("for example, 300", "например, 300"),
            &mut self.form.extension,
            false,
        );
        let password_response = field(
            ui,
            l.t("Password", "Пароль"),
            "",
            &mut self.form.password,
            true,
        );

        if let Some(error) = &self.form.error {
            ui.add_space(4.0);
            ui.label(RichText::new(error.text(l)).size(13.5).color(RED));
        }
        ui.add_space(20.0);

        let ready = !self.form.server.trim().is_empty()
            && !self.form.extension.trim().is_empty()
            && !self.form.password.is_empty();
        let enter = password_response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let label = if connecting {
            l.t("Connecting…", "Подключаемся…")
        } else {
            l.t("Sign in", "Войти")
        };
        if pill(
            ui,
            52.0,
            ACCENT,
            label,
            Color32::WHITE,
            ready && !connecting,
        ) && ready
            || (enter && ready && !connecting)
        {
            self.submit_login();
        }
        ui.add_space(10.0);
        let link = ui.allocate_space(vec2(ui.available_width(), 26.0)).1;
        let link_response = ui
            .interact(link, ui.id().with("login_settings"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.painter().text(
            link.center(),
            Align2::CENTER_CENTER,
            l.t("Connection settings", "Настройки подключения"),
            FontId::proportional(13.5),
            if link_response.hovered() {
                TEXT
            } else {
                ACCENT
            },
        );
        if link_response.clicked() {
            self.open_settings(SettingsTab::Network);
        }
        self.quit_footer(ui);
    }
}

fn field(
    ui: &mut Ui,
    label: &str,
    hint: &str,
    value: &mut String,
    password: bool,
) -> egui::Response {
    ui.label(RichText::new(label).size(13.0).color(MUTED));
    ui.add_space(4.0);
    let response = ui.add(
        TextEdit::singleline(value)
            .password(password)
            .hint_text(RichText::new(hint).color(MUTED.gamma_multiply(0.7)))
            .desired_width(f32::INFINITY)
            .margin(vec2(14.0, 12.0))
            .font(FontId::proportional(16.0)),
    );
    ui.add_space(14.0);
    response
}

// --------------------------------------------------------------- phone

impl PhoneApp {
    fn phone_screen(&mut self, ui: &mut Ui) {
        self.header(ui);
        ui.add_space(10.0);
        self.dnd_banner(ui);
        self.toast_banner(ui);
        self.tab_switch(ui);
        ui.add_space(12.0);
        match self.tab {
            Tab::Dial => self.dial_tab(ui),
            Tab::Recent => self.recent_tab(ui),
        }
        self.quit_footer(ui);
    }

    fn header(&mut self, ui: &mut Ui) {
        let l = self.lang();
        let (dot, status) = match &self.reg {
            RegState::Online => (GREEN, l.t("Online", "На связи").to_string()),
            RegState::Connecting => (AMBER, l.t("Connecting…", "Подключаемся…").to_string()),
            RegState::Failed { notice, retry } => (
                RED,
                if *retry {
                    l.t("No connection, trying again…", "Нет связи, пробуем снова…")
                        .to_string()
                } else {
                    notice.text(l)
                },
            ),
            RegState::Offline => (MUTED, l.t("Not connected", "Не подключено").to_string()),
        };
        let rect = ui.allocate_space(vec2(ui.available_width(), 44.0)).1;
        let painter = ui.painter();
        painter.circle_filled(pos2(rect.left() + 6.0, rect.center().y), 5.0, dot);
        painter.text(
            pos2(rect.left() + 20.0, rect.center().y - 9.0),
            Align2::LEFT_CENTER,
            format!("{} {}", l.t("Extension", "Номер"), self.stored.extension),
            FontId::proportional(15.0),
            TEXT,
        );
        painter.text(
            pos2(rect.left() + 20.0, rect.center().y + 9.0),
            Align2::LEFT_CENTER,
            status,
            FontId::proportional(12.5),
            MUTED,
        );
        // The settings button: a round gear at the right edge.
        let gear_rect =
            Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), vec2(32.0, 32.0));
        let gear = ui
            .interact(gear_rect, ui.id().with("open_settings"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(l.t("Settings", "Настройки"));
        let fill = if gear.hovered() { SURFACE_HI } else { SURFACE };
        ui.painter().circle_filled(gear_rect.center(), 16.0, fill);
        settings_screen::draw_gear(
            ui,
            gear_rect.center(),
            8.5,
            if gear.hovered() { TEXT } else { MUTED },
        );
        if gear.clicked() {
            self.open_settings(SettingsTab::Account);
        }
        let switch_rect = Rect::from_center_size(
            pos2(
                gear_rect.left() - 10.0 - LANG_SELECT_WIDTH / 2.0,
                rect.center().y,
            ),
            vec2(LANG_SELECT_WIDTH, 32.0),
        );
        if let Some(chosen) = language_select(ui, switch_rect, l) {
            self.set_language(chosen);
        }
        // The full explanation of a connection problem, when there is one.
        if let RegState::Failed { notice, .. } = &self.reg {
            let status_rect = Rect::from_min_size(
                pos2(rect.left() + 20.0, rect.center().y),
                vec2(switch_rect.left() - rect.left() - 28.0, 18.0),
            );
            ui.interact(status_rect, ui.id().with("status_detail"), Sense::hover())
                .on_hover_text(notice.text(l));
        }
    }

    fn toast_banner(&mut self, ui: &mut Ui) {
        let Some((notice, _)) = &self.toast else {
            return;
        };
        let message = notice.text(self.stored.language);
        egui::Frame::new()
            .fill(SURFACE_HI)
            .corner_radius(CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&message).size(13.5).color(TEXT));
            });
        ui.add_space(10.0);
    }

    fn tab_switch(&mut self, ui: &mut Ui) {
        let rect = ui.allocate_space(vec2(ui.available_width(), 40.0)).1;
        ui.painter()
            .rect_filled(rect, CornerRadius::same(20), SURFACE);
        let half = rect.width() / 2.0;
        let l = self.lang();
        for (i, (tab, label)) in [
            (Tab::Dial, l.t("Dial", "Набор")),
            (Tab::Recent, l.t("Recent", "Недавние")),
        ]
        .into_iter()
        .enumerate()
        {
            let tab_rect = Rect::from_min_size(
                pos2(rect.left() + half * i as f32, rect.top()),
                vec2(half, rect.height()),
            )
            .shrink(3.0);
            let response = ui.interact(tab_rect, ui.id().with(("tab", i)), Sense::click());
            let active = self.tab == tab;
            if active {
                ui.painter()
                    .rect_filled(tab_rect, CornerRadius::same(17), SURFACE_HI);
            }
            ui.painter().text(
                tab_rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(14.5),
                if active { TEXT } else { MUTED },
            );
            if response.clicked() {
                self.tab = tab;
            }
        }
    }

    fn dial_tab(&mut self, ui: &mut Ui) {
        let l = self.lang();
        ui.add_space(10.0);
        let display = ui.add(
            TextEdit::singleline(&mut self.number)
                .font(FontId::proportional(34.0))
                .horizontal_align(Align::Center)
                .hint_text(
                    RichText::new(l.t("Enter a number", "Введите номер"))
                        .size(22.0)
                        .color(MUTED.gamma_multiply(0.6)),
                )
                .desired_width(f32::INFINITY)
                .frame(egui::Frame::NONE)
                .margin(vec2(0.0, 10.0)),
        );
        self.number.retain(|c| "0123456789*#+".contains(c));
        display.request_focus();
        let enter = display.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

        // Erase is a small button under the number; while there is no number the space stays empty.
        let erase_row = ui.allocate_space(vec2(ui.available_width(), 26.0)).1;
        if !self.number.is_empty() {
            let rect = Rect::from_center_size(erase_row.center(), vec2(90.0, 24.0));
            let response = ui.interact(rect, ui.id().with("erase"), Sense::click());
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                l.t("Erase", "Стереть"),
                FontId::proportional(13.5),
                if response.hovered() { TEXT } else { MUTED },
            );
            if response.clicked() {
                self.number.pop();
            }
        }
        ui.add_space(6.0);

        if let Some(key) = keypad(ui, &DIAL_KEYS, 64.0, 14.0) {
            self.number.push_str(key);
        }
        ui.add_space(14.0);

        let can_call = !self.number.trim().is_empty() && matches!(self.reg, RegState::Online);
        let clicked = pill(
            ui,
            54.0,
            GREEN,
            l.t("Call", "Позвонить"),
            Color32::WHITE,
            can_call,
        );
        if (clicked || enter) && can_call {
            let number = self.number.clone();
            self.start_call(&number);
        }
        if !matches!(self.reg, RegState::Online) {
            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(l.t(
                        "You can call once the phone is online",
                        "Звонить можно, когда телефон на связи",
                    ))
                    .size(12.5)
                    .color(MUTED),
                );
            });
        }
    }

    fn recent_tab(&mut self, ui: &mut Ui) {
        let l = self.lang();
        if self.stored.history.is_empty() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(l.t("No calls yet", "Звонков пока не было"))
                        .size(16.0)
                        .color(TEXT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(l.t("Your calls will appear here", "Здесь появятся ваши звонки"))
                        .size(13.5)
                        .color(MUTED),
                );
            });
            return;
        }
        let mut chosen: Option<String> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            // Leave room for the "Quit app" link at the bottom.
            .max_height((ui.available_height() - 44.0).max(100.0))
            .show(ui, |ui| {
                for (i, entry) in self.stored.history.iter().enumerate() {
                    let rect = ui.allocate_space(vec2(ui.available_width(), 58.0)).1;
                    let response = ui.interact(rect, ui.id().with(("recent", i)), Sense::click());
                    if response.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(10), SURFACE);
                    }
                    let (color, kind) = history_kind(entry, l);
                    let painter = ui.painter();
                    painter.circle_filled(pos2(rect.left() + 14.0, rect.center().y), 5.0, color);
                    painter.text(
                        pos2(rect.left() + 32.0, rect.center().y - 10.0),
                        Align2::LEFT_CENTER,
                        peer_label(&entry.number, l),
                        FontId::proportional(16.0),
                        if entry.outcome == Outcome::Missed {
                            RED
                        } else {
                            TEXT
                        },
                    );
                    painter.text(
                        pos2(rect.left() + 32.0, rect.center().y + 11.0),
                        Align2::LEFT_CENTER,
                        format!("{kind} · {}", format_time(entry.started_at, l)),
                        FontId::proportional(12.5),
                        MUTED,
                    );
                    if entry.duration_secs > 0 {
                        painter.text(
                            pos2(rect.right() - 8.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            format_duration(entry.duration_secs, l),
                            FontId::proportional(13.0),
                            MUTED,
                        );
                    }
                    if response.clicked() {
                        chosen = Some(entry.number.clone());
                    }
                }
            });
        if let Some(number) = chosen.filter(|n| n != UNKNOWN_PEER) {
            // A history entry may hold a name with the number, "Name (300)": dial the part in brackets.
            let dialable = match (number.rfind('('), number.rfind(')')) {
                (Some(a), Some(b)) if a < b => number[a + 1..b].to_string(),
                _ => number,
            };
            self.number = dialable;
            self.tab = Tab::Dial;
        }
    }
}

fn history_kind(entry: &HistoryEntry, l: Lang) -> (Color32, &'static str) {
    match (entry.direction, entry.outcome) {
        (Direction::Incoming, Outcome::Missed) => (RED, l.t("Missed", "Пропущенный")),
        (Direction::Incoming, Outcome::Declined) => (MUTED, l.t("Declined", "Отклонённый")),
        (Direction::Incoming, _) => (ACCENT, l.t("Incoming", "Входящий")),
        (Direction::Outgoing, Outcome::Failed) => (MUTED, l.t("No answer", "Не дозвонились")),
        (Direction::Outgoing, Outcome::Cancelled) => (MUTED, l.t("Cancelled", "Отменённый")),
        (Direction::Outgoing, _) => (GREEN, l.t("Outgoing", "Исходящий")),
    }
}

/// The caller's number, or a localized placeholder when the caller was unknown.
fn peer_label(peer: &str, l: Lang) -> &str {
    if peer == UNKNOWN_PEER {
        l.t("Unknown number", "Неизвестный номер")
    } else {
        peer
    }
}

fn format_time(unix: i64, l: Lang) -> String {
    use chrono::{Local, TimeZone};
    let Some(time) = Local.timestamp_opt(unix, 0).single() else {
        return String::new();
    };
    let today = Local::now().date_naive();
    let day = time.date_naive();
    if day == today {
        time.format("%H:%M").to_string()
    } else if today.pred_opt() == Some(day) {
        format!("{}, {}", l.t("yesterday", "вчера"), time.format("%H:%M"))
    } else if l == Lang::English {
        time.format("%b %-d, %H:%M").to_string()
    } else {
        time.format("%d.%m, %H:%M").to_string()
    }
}

fn format_duration(secs: u64, l: Lang) -> String {
    let (h, min, s) = (l.t("h", "ч"), l.t("min", "мин"), l.t("s", "с"));
    if secs >= 3600 {
        format!("{} {h} {:02} {min}", secs / 3600, secs % 3600 / 60)
    } else if secs >= 60 {
        format!("{} {min} {:02} {s}", secs / 60, secs % 60)
    } else {
        format!("{secs} {s}")
    }
}

// ---------------------------------------------------------------- call

impl PhoneApp {
    fn call_screen(&mut self, ui: &mut Ui) {
        let Some(call) = self.call.clone() else {
            return;
        };
        let l = self.lang();

        ui.add_space(28.0);
        let avatar = ui.allocate_space(vec2(ui.available_width(), 92.0)).1;
        draw_avatar(ui, avatar.center(), 44.0);

        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(peer_label(&call.peer, l))
                    .size(28.0)
                    .strong()
                    .color(TEXT),
            );
            ui.add_space(4.0);
            let timer = call
                .connected_at
                .map(|t| format_clock(t.elapsed().as_secs()))
                .unwrap_or_default();
            let (status, color) = match call.phase {
                Phase::Dialing => (l.t("Dialing…", "Набираем…").to_string(), MUTED),
                Phase::Ringing => (l.t("Ringing…", "Идёт вызов…").to_string(), MUTED),
                Phase::Incoming => (l.t("Incoming call", "Входящий звонок").to_string(), ACCENT),
                Phase::Active if call.transferring => (
                    format!("{timer} · {}", l.t("Transferring…", "Переводим…")),
                    ACCENT,
                ),
                Phase::Active if call.remote_hold => (
                    format!(
                        "{timer} · {}",
                        l.t("On hold by the other side", "Вас поставили на удержание")
                    ),
                    AMBER,
                ),
                Phase::Active if call.local_hold => (
                    format!("{timer} · {}", l.t("On hold", "На удержании")),
                    AMBER,
                ),
                Phase::Active => (timer, GREEN),
            };
            ui.label(RichText::new(status).size(16.0).color(color));
        });

        ui.add_space(16.0);
        match call.phase {
            Phase::Incoming => {
                let rect = ui.allocate_space(vec2(ui.available_width(), 56.0)).1;
                let (left, right) = split_row(rect, 12.0);
                if pill_in(
                    ui,
                    left,
                    "reject",
                    RED,
                    l.t("Decline", "Отклонить"),
                    Color32::WHITE,
                    true,
                ) {
                    self.send(Command::Reject);
                }
                if pill_in(
                    ui,
                    right,
                    "answer",
                    GREEN,
                    l.t("Answer", "Ответить"),
                    Color32::WHITE,
                    true,
                ) {
                    self.send(Command::Answer);
                }
            }
            Phase::Active => {
                // Two rows of two buttons: mute and hold, keypad and transfer.
                let rect = ui.allocate_space(vec2(ui.available_width(), 44.0)).1;
                let (left, right) = split_row(rect, 12.0);
                let mute_label = if call.muted {
                    l.t("Unmute", "Включить микрофон")
                } else {
                    l.t("Mute", "Без звука")
                };
                let mute_fill = if call.muted { AMBER } else { SURFACE_HI };
                let mute_text = if call.muted { Color32::BLACK } else { TEXT };
                if pill_in(ui, left, "mute", mute_fill, mute_label, mute_text, true) {
                    self.send(Command::SetMute(!call.muted));
                }
                let hold_label = if call.local_hold {
                    l.t("Resume", "Вернуть")
                } else {
                    l.t("Hold", "Удержать")
                };
                let hold_fill = if call.local_hold { AMBER } else { SURFACE_HI };
                let hold_text = if call.local_hold {
                    Color32::BLACK
                } else {
                    TEXT
                };
                if pill_in(ui, right, "hold", hold_fill, hold_label, hold_text, true) {
                    self.send(Command::Hold(!call.local_hold));
                }
                ui.add_space(10.0);
                let rect = ui.allocate_space(vec2(ui.available_width(), 44.0)).1;
                let (left, right) = split_row(rect, 12.0);
                let pad_fill = if self.keypad_open { ACCENT } else { SURFACE_HI };
                if pill_in(
                    ui,
                    left,
                    "pad",
                    pad_fill,
                    l.t("Keypad", "Клавиши"),
                    TEXT,
                    true,
                ) {
                    self.keypad_open = !self.keypad_open;
                    self.transfer_open = false;
                }
                let transfer_fill = if self.transfer_open {
                    ACCENT
                } else {
                    SURFACE_HI
                };
                if pill_in(
                    ui,
                    right,
                    "transfer",
                    transfer_fill,
                    l.t("Transfer", "Перевести"),
                    TEXT,
                    !call.transferring,
                ) {
                    self.transfer_open = !self.transfer_open;
                    self.keypad_open = false;
                }
                ui.add_space(12.0);
                if self.keypad_open {
                    if let Some(key) = keypad(ui, &DIAL_KEYS, 54.0, 10.0) {
                        if let Some(digit) = key.chars().next() {
                            self.send(Command::Dtmf(digit));
                        }
                    }
                    ui.add_space(10.0);
                }
                if self.transfer_open {
                    self.transfer_panel(ui, l);
                    ui.add_space(10.0);
                }
                if pill(
                    ui,
                    54.0,
                    RED,
                    l.t("End call", "Завершить"),
                    Color32::WHITE,
                    true,
                ) {
                    self.send(Command::Hangup);
                }
            }
            Phase::Dialing | Phase::Ringing => {
                if pill(
                    ui,
                    54.0,
                    RED,
                    l.t("Cancel", "Отменить"),
                    Color32::WHITE,
                    true,
                ) {
                    self.send(Command::Hangup);
                }
            }
        }
    }
}

impl PhoneApp {
    /// Asks where to transfer the call to and sends the request.
    fn transfer_panel(&mut self, ui: &mut Ui, l: Lang) {
        let field = ui.add(
            TextEdit::singleline(&mut self.transfer_target)
                .hint_text(
                    RichText::new(l.t(
                        "number or address to transfer to",
                        "номер или адрес, куда перевести",
                    ))
                    .color(MUTED.gamma_multiply(0.7)),
                )
                .desired_width(f32::INFINITY)
                .margin(vec2(14.0, 12.0))
                .font(FontId::proportional(16.0)),
        );
        field.request_focus();
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        ui.add_space(8.0);
        let ready = !self.transfer_target.trim().is_empty();
        if (pill(
            ui,
            44.0,
            ACCENT,
            l.t("Transfer now", "Перевести"),
            Color32::WHITE,
            ready,
        ) || enter)
            && ready
        {
            let target = std::mem::take(&mut self.transfer_target);
            self.send(Command::Transfer(target.trim().to_string()));
            self.transfer_open = false;
        }
    }

    /// A reminder, above the dialer, that incoming calls are being turned away.
    fn dnd_banner(&mut self, ui: &mut Ui) {
        if !self.stored.calls.do_not_disturb {
            return;
        }
        let l = self.lang();
        let rect = ui.allocate_space(vec2(ui.available_width(), 38.0)).1;
        ui.painter()
            .rect_filled(rect, CornerRadius::same(12), AMBER.gamma_multiply(0.16));
        ui.painter().text(
            pos2(rect.left() + 14.0, rect.center().y),
            Align2::LEFT_CENTER,
            l.t("Do not disturb is on", "«Не беспокоить» включён"),
            FontId::proportional(13.5),
            AMBER,
        );
        let off =
            Rect::from_center_size(pos2(rect.right() - 44.0, rect.center().y), vec2(80.0, 28.0));
        let response = ui
            .interact(off, ui.id().with("dnd_off"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.painter().text(
            off.center(),
            Align2::CENTER_CENTER,
            l.t("Turn off", "Выключить"),
            FontId::proportional(13.5),
            if response.hovered() { TEXT } else { ACCENT },
        );
        if response.clicked() {
            self.stored.calls.do_not_disturb = false;
            store::save(&self.stored);
            self.send(Command::SetCalls(self.stored.calls.clone()));
        }
        ui.add_space(10.0);
    }
}

fn format_clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

fn draw_avatar(ui: &Ui, center: Pos2, radius: f32) {
    let rect = Rect::from_center_size(center, Vec2::splat(radius * 2.0));
    let painter = ui.painter().with_clip_rect(rect);
    painter.circle_filled(center, radius, SURFACE_HI);
    painter.circle_filled(
        pos2(center.x, center.y - radius * 0.2),
        radius * 0.28,
        MUTED,
    );
    painter.circle_filled(
        pos2(center.x, center.y + radius * 0.85),
        radius * 0.55,
        MUTED,
    );
}

// -------------------------------------------------------------- widgets

/// Three-column digit keypad. Returns the key that was pressed.
fn keypad(ui: &mut Ui, keys: &[&'static str], diameter: f32, gap: f32) -> Option<&'static str> {
    let rows = keys.len().div_ceil(3);
    let width = diameter * 3.0 + gap * 2.0;
    let height = diameter * rows as f32 + gap * (rows as f32 - 1.0);
    let available = ui.available_width();
    let (_, area) = ui.allocate_space(vec2(available, height));
    let left = area.center().x - width / 2.0;

    let mut pressed = None;
    for (i, key) in keys.iter().enumerate() {
        let (row, col) = (i / 3, i % 3);
        let center = pos2(
            left + diameter / 2.0 + col as f32 * (diameter + gap),
            area.top() + diameter / 2.0 + row as f32 * (diameter + gap),
        );
        let rect = Rect::from_center_size(center, Vec2::splat(diameter));
        let response = ui.interact(rect, ui.id().with(("key", i)), Sense::click());
        let fill = if response.is_pointer_button_down_on() {
            ACCENT.gamma_multiply(0.5)
        } else if response.hovered() {
            SURFACE_HI
        } else {
            SURFACE
        };
        ui.painter().circle_filled(center, diameter / 2.0, fill);
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            key,
            FontId::proportional(26.0),
            TEXT,
        );
        if response.clicked() {
            pressed = Some(*key);
        }
    }
    pressed
}

/// A full-width pill button. Returns true when clicked.
fn pill(
    ui: &mut Ui,
    height: f32,
    fill: Color32,
    label: &str,
    text: Color32,
    enabled: bool,
) -> bool {
    let rect = ui.allocate_space(vec2(ui.available_width(), height)).1;
    pill_in(ui, rect, label, fill, label, text, enabled)
}

fn pill_in(
    ui: &mut Ui,
    rect: Rect,
    id: &str,
    fill: Color32,
    label: &str,
    text: Color32,
    enabled: bool,
) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, ui.id().with(("pill", id)), sense);
    let mut color = fill;
    if !enabled {
        color = fill.gamma_multiply(0.35);
    } else if response.is_pointer_button_down_on() {
        color = fill.gamma_multiply(0.8);
    } else if response.hovered() {
        color = fill.gamma_multiply(1.12);
    }
    ui.painter()
        .rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), color);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(if rect.height() > 50.0 { 17.0 } else { 15.0 }),
        if enabled {
            text
        } else {
            text.gamma_multiply(0.6)
        },
    );
    enabled && response.clicked()
}

fn split_row(rect: Rect, gap: f32) -> (Rect, Rect) {
    let half = (rect.width() - gap) / 2.0;
    (
        Rect::from_min_size(rect.min, vec2(half, rect.height())),
        Rect::from_min_size(
            pos2(rect.min.x + half + gap, rect.min.y),
            vec2(half, rect.height()),
        ),
    )
}

fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|style| {
        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = BG;
        v.extreme_bg_color = SURFACE;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.5);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.widgets.inactive.corner_radius = CornerRadius::same(12);
        v.widgets.hovered.corner_radius = CornerRadius::same(12);
        v.widgets.active.corner_radius = CornerRadius::same(12);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, SURFACE_HI);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        style.spacing.item_spacing = vec2(8.0, 6.0);
    });
}

/// Width of the language select. The open list is exactly as wide, so there is no empty space.
const LANG_SELECT_WIDTH: f32 = 64.0;
const LANG_ROW_HEIGHT: f32 = 34.0;
const LANG_LIST_MARGIN: f32 = 4.0;

/// Language drop-down (a select), drawn by hand so it matches the rest of the interface.
///
/// Both the closed pill and the open list show only the short language codes ("EN", "RU").
/// The list opens below the pill, aligned with it, and is exactly as wide; hovering a code
/// shows the language's full name. Returns the language the user picked this frame, if it changed.
fn language_select(ui: &mut Ui, rect: Rect, current: Lang) -> Option<Lang> {
    let button = ui
        .interact(rect, ui.id().with("language"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&button));

    // The closed pill.
    let fill = if open || button.hovered() {
        SURFACE_HI
    } else {
        SURFACE
    };
    let radius = CornerRadius::same((rect.height() / 2.0) as u8);
    ui.painter().rect_filled(rect, radius, fill);
    if open {
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().text(
        pos2(rect.left() + 13.0, rect.center().y),
        Align2::LEFT_CENTER,
        current.code(),
        FontId::proportional(13.5),
        TEXT,
    );
    draw_chevron(ui, pos2(rect.right() - 16.0, rect.center().y), open);

    // The open list.
    let mut picked = None;
    let anchor = pos2(rect.right() - LANG_SELECT_WIDTH, rect.bottom() + 6.0);
    let frame = egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, SURFACE_HI))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(LANG_LIST_MARGIN as i8))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(120),
        });
    egui::Popup::from_toggle_button_response(&button)
        .at_position(anchor)
        .gap(0.0)
        .width(LANG_SELECT_WIDTH)
        .frame(frame)
        .show(|ui| {
            ui.set_width(LANG_SELECT_WIDTH - 2.0 * LANG_LIST_MARGIN);
            ui.spacing_mut().item_spacing.y = 2.0;
            for lang in Lang::ALL {
                if language_row(ui, lang, lang == current) {
                    picked = Some(lang);
                }
            }
        });
    picked.filter(|lang| *lang != current)
}

/// One row of the language list. Returns true when clicked.
fn language_row(ui: &mut Ui, lang: Lang, selected: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), LANG_ROW_HEIGHT), Sense::click());
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(lang.name());
    let radius = CornerRadius::same(10);
    if response.hovered() {
        ui.painter().rect_filled(rect, radius, SURFACE_HI);
    } else if selected {
        ui.painter()
            .rect_filled(rect, radius, ACCENT.gamma_multiply(0.14));
    }
    // 8 px of padding plus the 4 px list margin lines the code up with the one in the closed pill.
    ui.painter().text(
        pos2(rect.left() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        lang.code(),
        FontId::proportional(13.5),
        if selected { ACCENT } else { TEXT },
    );
    response.clicked()
}

/// A small chevron: pointing down when closed, up when open.
fn draw_chevron(ui: &Ui, center: Pos2, open: bool) {
    let dy = if open { -2.5 } else { 2.5 };
    let stroke = Stroke::new(1.6, MUTED);
    let points = [
        pos2(center.x - 4.5, center.y - dy),
        pos2(center.x, center.y + dy),
        pos2(center.x + 4.5, center.y - dy),
    ];
    ui.painter().line_segment([points[0], points[1]], stroke);
    ui.painter().line_segment([points[1], points[2]], stroke);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the language select in a headless egui context, one frame per call.
    struct Harness {
        ctx: egui::Context,
        frame: u32,
        rect: Rect,
    }

    impl Harness {
        fn new() -> Self {
            Harness {
                ctx: egui::Context::default(),
                frame: 0,
                rect: Rect::from_min_size(pos2(200.0, 20.0), vec2(LANG_SELECT_WIDTH, 32.0)),
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>, current: Lang) -> (Option<Lang>, bool) {
            self.frame += 1;
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 700.0))),
                time: Some(self.frame as f64 * 0.05),
                events,
                ..Default::default()
            };
            let rect = self.rect;
            let mut picked = None;
            let mut output = self.ctx.run_ui(raw, |ui| {
                picked = language_select(ui, rect, current);
            });
            // There is no renderer here, so there is nothing to upload the textures to.
            output.textures_delta.clear();
            let open = egui::Popup::is_any_open(&self.ctx);
            (picked, open)
        }

        fn click(&mut self, at: Pos2, current: Lang) -> Option<Lang> {
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.frame(vec![egui::Event::PointerMoved(at)], current);
            self.frame(vec![button(true)], current);
            let (picked, _) = self.frame(vec![button(false)], current);
            picked
        }

        fn settle(&mut self, current: Lang) {
            for _ in 0..3 {
                self.frame(vec![], current);
            }
        }
    }

    #[test]
    fn clicking_the_pill_opens_the_list_and_a_row_picks_a_language() {
        let mut h = Harness::new();
        h.settle(Lang::English);
        assert!(!egui::Popup::is_any_open(&h.ctx), "closed at first");

        h.click(h.rect.center(), Lang::English);
        h.settle(Lang::English);
        assert!(egui::Popup::is_any_open(&h.ctx), "the list opens on click");

        // Rows sit under the pill, aligned with it: the second row is "RU".
        let list_left = h.rect.right() - LANG_SELECT_WIDTH;
        let second_row = pos2(
            list_left + LANG_SELECT_WIDTH / 2.0,
            h.rect.bottom()
                + 6.0
                + LANG_LIST_MARGIN
                + LANG_ROW_HEIGHT
                + 2.0
                + LANG_ROW_HEIGHT / 2.0,
        );
        let picked = h.click(second_row, Lang::English);
        assert_eq!(picked, Some(Lang::Russian));
        h.settle(Lang::Russian);
        assert!(
            !egui::Popup::is_any_open(&h.ctx),
            "the list closes after picking"
        );
    }

    #[test]
    fn picking_the_current_language_changes_nothing() {
        let mut h = Harness::new();
        h.settle(Lang::English);
        h.click(h.rect.center(), Lang::English);
        h.settle(Lang::English);
        let list_left = h.rect.right() - LANG_SELECT_WIDTH;
        let first_row = pos2(
            list_left + LANG_SELECT_WIDTH / 2.0,
            h.rect.bottom() + 6.0 + LANG_LIST_MARGIN + LANG_ROW_HEIGHT / 2.0,
        );
        assert_eq!(h.click(first_row, Lang::English), None);
    }

    #[test]
    fn history_labels_exist_in_both_languages() {
        let entry = HistoryEntry {
            number: "100".into(),
            direction: Direction::Incoming,
            outcome: Outcome::Missed,
            started_at: 0,
            duration_secs: 0,
        };
        assert_eq!(history_kind(&entry, Lang::English).1, "Missed");
        assert_eq!(history_kind(&entry, Lang::Russian).1, "Пропущенный");
        assert_eq!(format_duration(65, Lang::English), "1 min 05 s");
        assert_eq!(format_duration(65, Lang::Russian), "1 мин 05 с");
        assert_eq!(peer_label(UNKNOWN_PEER, Lang::English), "Unknown number");
    }
}
