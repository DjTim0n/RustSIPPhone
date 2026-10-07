<div align="center">

<img src="assets/icon.png" alt="RustSIPPhone icon" width="128" height="128">

# RustSIPPhone 1.0

**A free, open-source desktop SIP phone written in Rust.**
Sign in to your PBX, dial a number, answer calls — on macOS, Windows and Linux.

[![Build](https://github.com/DjTim0n/RustSIPPhone/actions/workflows/build.yml/badge.svg)](https://github.com/DjTim0n/RustSIPPhone/actions/workflows/build.yml)
[![Release](https://img.shields.io/github/v/release/DjTim0n/RustSIPPhone?display_name=tag&color=4C8DFF)](https://github.com/DjTim0n/RustSIPPhone/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-2FB35A.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Windows%20%7C%20Linux-8C96A5)
![Rust](https://img.shields.io/badge/rust-2024%20edition-E5484D)

[Download](#-download) · [Quick start](#-quick-start) · [Verify](#-verify-your-download) · [Build from source](#-build-from-source) · [License](#-license)

</div>

---

## ✨ Features

- **Outgoing and incoming calls** over SIP, with digest authentication.
- **Sign in inside the app** — no config files, no command line. Your password is kept in the system keychain.
- **Dialer** with an on-screen keypad and keyboard input.
- **Recent calls** — incoming, outgoing and missed, one click to call back.
- **In-call controls** — mute, keypad tones (DTMF) for voice menus, hang up.
- **Ringtone** that plays on every platform, with nothing extra to install.
- **Runs in the background.** Closing the window does not quit the app, so the phone stays registered and can still ring. On macOS the window simply goes away, as in any Mac app, and a click on the Dock icon brings it back; on Windows and Linux it is minimized to the taskbar. An incoming call restores the window, raises it above other windows, gives it focus and bounces the Dock icon / flashes the taskbar button.
- **Two interface languages:** **English** (the default) and **Russian**. Pick it from a drop-down list on the sign-in screen or in the top bar at any time; the choice is remembered.
- **Single small binary.** No runtime, no installer required.

## 📦 Download

Grab the package for your system from the **[latest release](https://github.com/DjTim0n/RustSIPPhone/releases/latest)**.

| System | Package | What's inside |
| --- | --- | --- |
| 🍎 **macOS** 11+ (Apple Silicon and Intel) | [`RustSIPPhone-1.0.1.dmg`](https://github.com/DjTim0n/RustSIPPhone/releases/download/v1.0.1/RustSIPPhone-1.0.1.dmg) | Disk image with `RustSIPPhone.app` |
| 🪟 **Windows** 10+ (64-bit) | [`RustSIPPhone-windows.zip`](https://github.com/DjTim0n/RustSIPPhone/releases/download/v1.0.1/RustSIPPhone-windows.zip) | `rust_sip_phone.exe` |
| 🐧 **Linux** (x86_64) | [`RustSIPPhone-linux-x86_64.tar.gz`](https://github.com/DjTim0n/RustSIPPhone/releases/download/v1.0.1/RustSIPPhone-linux-x86_64.tar.gz) | Binary, desktop entry and icon |

Every release also includes [`SHA256SUMS.txt`](https://github.com/DjTim0n/RustSIPPhone/releases/download/v1.0.1/SHA256SUMS.txt) so you can [verify your download](#-verify-your-download).

## 🚀 Quick start

### macOS

1. Open the `.dmg` and drag **RustSIPPhone** into **Applications**.
2. Open it from Applications. The first time, macOS may say the app is from an unidentified developer — the app is open source and signed ad-hoc, not notarized. **Right-click the app → Open → Open**.
   Or from Terminal:
   ```bash
   xattr -dr com.apple.quarantine /Applications/RustSIPPhone.app
   ```
3. Allow **Microphone** access when asked. Without it calls cannot start.
4. Allow **Keychain** access if prompted — that is where your password is stored.

### Windows

1. Unzip `RustSIPPhone-windows.zip` anywhere (for example `C:\Tools\RustSIPPhone`).
2. Double-click `rust_sip_phone.exe`.
3. If **Windows protected your PC** appears: **More info → Run anyway**.
4. Allow microphone access when Windows asks (Settings → Privacy & security → Microphone).

### Linux

```bash
tar -xzf RustSIPPhone-linux-x86_64.tar.gz
cd RustSIPPhone
./rust_sip_phone
```

You need ALSA, a graphical session (X11 or Wayland) and, to remember your password, a keyring service (GNOME Keyring or KWallet). On Debian/Ubuntu:

```bash
sudo apt install libasound2 libxkbcommon0 libwayland-client0 gnome-keyring
```

Optional — add it to your application menu:

```bash
mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/icons
cp rust_sip_phone ~/.local/bin/
cp rustsipphone.png ~/.local/share/icons/
cp rustsipphone.desktop ~/.local/share/applications/
```

No keyring service? The phone still works; it just asks for the password each time you start it.

### First launch

1. Open the language drop-down in the top-right corner if you want something other than English.
2. Enter the **station address** (your PBX, for example `192.168.1.10:5060`; the port defaults to `5060`), your **extension number** and your **password** — your administrator or provider gives you these.
3. Press **Sign in**. When the dot in the top-left turns green and says **Online**, you are registered.
4. Type or tap a number and press **Call**.
5. Incoming calls ring and open an **Answer / Decline** screen, even if the window was closed or minimized.
6. To really quit, press **Quit app** at the bottom of the window (or use **⌘Q** on macOS). Closing the window with the red button / **×** only sends it to the background.

## ✅ Verify your download

**1. Check the checksum.** Download `SHA256SUMS.txt` next to your package, then:

```bash
# macOS
shasum -a 256 -c SHA256SUMS.txt --ignore-missing

# Linux
sha256sum -c SHA256SUMS.txt --ignore-missing
```

```powershell
# Windows (PowerShell) — compare the output with the line in SHA256SUMS.txt
Get-FileHash .\RustSIPPhone-windows.zip -Algorithm SHA256
```

You should see `OK` next to your file name.

**2. Check the macOS signature.**

```bash
codesign --verify --deep --strict --verbose=2 /Applications/RustSIPPhone.app
codesign -dv /Applications/RustSIPPhone.app 2>&1 | grep -E "Identifier|Signature"
# Identifier=com.rustsipphone.app
# Signature=adhoc
```

**3. Check that it works.**

| Step | Expected result |
| --- | --- |
| Sign in with your account | Green dot and **Online** |
| Call a second extension | **Ringing…**, then a timer when answered |
| Call an echo-test number (on Asterisk usually `*43` or `600`) | You hear yourself, both directions work |
| Call the phone from another extension | Ringtone and the answer screen |
| Press **Keypad** during a call and dial a digit | Voice menus react to the tone |

If a call connects but you hear nothing, the app tells you when no audio arrived from the other side — that usually points to a NAT or firewall problem (see [Notes](#-notes-and-limitations)).

## 🛠 Build from source

You need [Rust](https://rustup.rs) **1.85 or newer** (the project uses the 2024 edition).

```bash
git clone https://github.com/DjTim0n/RustSIPPhone.git
cd RustSIPPhone
cargo run --release
```

**Linux build dependencies** (Debian/Ubuntu):

```bash
sudo apt install pkg-config libasound2-dev libxkbcommon-dev libwayland-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgtk-3-dev libssl-dev
```

**Run the tests:**

```bash
cargo test
```

**Package for your system:**

| Target | Command | Result |
| --- | --- | --- |
| macOS `.app` | `./scripts/bundle-macos.sh` | `dist/RustSIPPhone.app` |
| macOS `.app` + `.dmg` | `./scripts/bundle-macos.sh --dmg` | `dist/RustSIPPhone-1.0.1.dmg` |
| macOS universal (Intel + Apple Silicon) | `./scripts/bundle-macos.sh --universal --dmg` | one app for both |
| Windows / Linux | `cargo build --release` | `target/release/rust_sip_phone[.exe]` |

Set `SIGN_IDENTITY="Developer ID Application: …"` to sign with your own Apple certificate instead of the default ad-hoc signature.

Releases are built by [GitHub Actions](.github/workflows/build.yml) on macOS, Windows and Linux. Push a tag like `v1.0.0` and the workflow publishes the packages and checksums.

## 🧭 How it works

```
 ┌────────────┐   commands    ┌────────────┐  SIP (UDP)   ┌────────┐
 │  egui UI   │ ────────────▶ │   engine   │ ───────────▶ │  PBX   │
 │  ui.rs     │ ◀──────────── │ engine.rs  │ ◀─────────── │        │
 └────────────┘    events     └─────┬──────┘              └───┬────┘
                                    │ calls                   │
                              ┌─────▼──────┐   RTP (G.711)    │
                              │  call.rs   │ ◀────────────────┘
                              │  media.rs  │ ──▶ microphone / speaker (cpal)
                              └────────────┘
```

| Module | Role |
| --- | --- |
| [`ui.rs`](src/ui.rs) | Desktop interface (egui): sign-in, dialer, recent calls, call screen, language switch |
| [`i18n.rs`](src/i18n.rs) | Interface languages and the text of every message the core reports |
| [`engine.rs`](src/engine.rs) | SIP registration, incoming-request routing, command handling |
| [`call.rs`](src/call.rs) | Outgoing and incoming calls, call state |
| [`media.rs`](src/media.rs) | RTP send/receive, DTMF, protection against stray audio packets |
| [`audio.rs`](src/audio.rs), [`ringtone.rs`](src/ringtone.rs) | Microphone, speaker and ringtone through cpal |
| [`g711.rs`](src/g711.rs), [`rtp.rs`](src/rtp.rs), [`sdp.rs`](src/sdp.rs) | Codec, packet format and session description |
| [`store.rs`](src/store.rs) | Settings file, call history and keychain access |

The SIP signalling is handled by [`rsipstack`](https://crates.io/crates/rsipstack).

Your account, call history and language choice live in:

| System | Settings and history | Password |
| --- | --- | --- |
| macOS | `~/Library/Application Support/RustSIPPhone/settings.json` | Keychain |
| Windows | `%APPDATA%\RustSIPPhone\settings.json` | Credential Manager |
| Linux | `~/.config/RustSIPPhone/settings.json` | Secret Service (GNOME Keyring, KWallet) |

## 📝 Notes and limitations

- **One account, one call at a time.**
- **Fixed-size window:** it cannot be resized or maximized.
- **Transport:** SIP over UDP, IPv4. No TCP/TLS yet.
- **Audio:** G.711 (PCMU/PCMA) only. Calls are **not encrypted** (no SRTP); use a trusted network or a VPN.
- **Not supported yet:** hold, transfer, video.
- **NAT:** works when the PBX is on your network or reachable publicly. If you are behind NAT and the other side hears nothing, check that your router does not block UDP media from the PBX.
- **Safety:** the phone only accepts incoming calls from the server you signed in to, and only accepts audio from the addresses negotiated for the call.
- Developed mainly on macOS; Windows and Linux builds come from CI. Please [open an issue](https://github.com/DjTim0n/RustSIPPhone/issues) if something misbehaves there.

## 🤝 Contributing

Issues and pull requests are welcome. Please run `cargo test` before sending a change and keep the code formatted with `cargo fmt`.

**Adding a language:** add a variant to `Lang` in [`i18n.rs`](src/i18n.rs), then supply its text where `Lang::t` is used (the interface in [`ui.rs`](src/ui.rs) and the messages in [`i18n.rs`](src/i18n.rs)). A test checks that every message exists in every language.

## 📄 License

RustSIPPhone is open source software released under the **[MIT License](LICENSE)** — you may use, copy, modify and distribute it freely, including commercially, as long as the copyright notice is kept.

Copyright © 2026 Tim

<div align="center">
<sub>Built with Rust 🦀 · <a href="https://github.com/emilk/egui">egui</a> · <a href="https://github.com/RustAudio/cpal">cpal</a> · <a href="https://github.com/restsend/rsipstack">rsipstack</a></sub>
</div>
