<div align="center">

<img src="https://raw.githubusercontent.com/PookiePepelsss/MultiRoblox-RAM/main/MultiRoblox/src-tauri/icons/128x128.png" width="88" />

# MultiRoblox

**Run as many Roblox accounts as you want, all at once, on Windows.**

[![platform](https://img.shields.io/badge/platform-windows-0078D6?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/PookiePepelsss/MultiRoblox-RAM/releases)
[![built with tauri](https://img.shields.io/badge/built%20with-tauri-24C8DB?style=for-the-badge&logo=tauri&logoColor=white)](https://tauri.app/)
[![license](https://img.shields.io/badge/license-MIT-3DDC97?style=for-the-badge)](LICENSE)
[![latest release](https://img.shields.io/github/v/tag/PookiePepelsss/MultiRoblox-RAM?style=for-the-badge&label=latest&color=FF6B6B)](https://github.com/PookiePepelsss/MultiRoblox-RAM/releases)

<br>

<!-- add a screenshot or GIF of the app here -->

</div>

---

## Quick start

No installer needed. Grab the exe from [**Releases**](https://github.com/PookiePepelsss/MultiRoblox-RAM/releases) and run it.

Or build it yourself:

```bash
git clone https://github.com/PookiePepelsss/MultiRoblox-RAM.git
cd MultiRoblox-RAM/MultiRoblox
build.bat
```

> Requires the [Rust toolchain](https://rustup.rs). The finished exe lands in `dist\MultiRoblox.exe`.

---

## Features

### 👤 Accounts
- Launch as many accounts side by side as you want
- Sign in through a real Chrome window, or paste a cookie directly
- Set a game ID or private server link per account so it launches straight there
- Nicknames, search, and filtering across all saved accounts
- Cookies encrypted with AES-256-GCM, stored locally. Nothing leaves your device
- Auto-relaunch on unexpected disconnect, back into the same game (crash-loop protected)

### 📦 Packages
- Group accounts into packages (farm squad, trading alts, etc.)
- Launch an entire group at once, with a shared or per-account game target

### 🎚️ Mixer
- Render quality, FPS target, and volume for all running instances, one panel
- Graphics quality writes to Roblox's fast flags; FPS cap writes Roblox's global client settings
- Works with vanilla Roblox and Bloxstrap / Froststrap / Voidstrap / Fishstrap
- Live, per-instance OS-level volume control
- Kill all instances with one click

### 📸 Tracking
- Automated per-account screenshots sent to a Discord webhook on a timer
- Outline one or more capture spots per account, or grab the full window

### 📊 Charts
- Browse top playing now, top rated, and top earning games
- Search and launch any game straight from the charts page

### 🎲 Generator
- Generate Roblox accounts via a [bloxgen.net](https://bloxgen.net/) API key

### ⚙️ Settings
- General, themes, and sounds tabs
- Custom encryption key (AES-256-GCM) or OS-native DPAPI (Windows keychain)
- Custom UI sound profiles, with volume control and upload-your-own support
- Light/dark theme, built-in update checker

### 🕹️ Anti-AFK
- Taps a benign key into every open Roblox window on a configurable interval, so the idle kick never fires

### 📜 Logs
- Real-time log viewer with in-page search (Ctrl+F)

---

## How it works

Roblox prevents multiple instances by holding a Windows mutex. MultiRoblox grabs that mutex first through a lightweight native helper (`RobloxNative.exe`, written in C#), so Roblox opens a fresh instance every time. Each account gets its own auth ticket before launch, so they all sign in as different accounts.

Browser login downloads a private copy of Chrome on first use and won't touch your existing install.

If the native helper isn't shipped with a build, it compiles from the bundled source using the .NET Framework `csc.exe` already on every Windows machine.

---

<div align="center">

MIT License. See [LICENSE](LICENSE)

</div>
