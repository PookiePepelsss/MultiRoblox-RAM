<div align="center">

# MultiRoblox

**run as many roblox accounts as you want, all at once, on windows**

[![platform](https://img.shields.io/badge/platform-windows-blue?style=flat-square&logo=windows)](https://github.com/PookiePepelss/multiroblox/releases)
[![built with electron](https://img.shields.io/badge/built%20with-electron-47848f?style=flat-square&logo=electron)](https://www.electronjs.org/)
[![license](https://img.shields.io/badge/license-MIT-green?style=flat-square)](LICENSE)
[![download](https://img.shields.io/badge/download-latest%20release-brightgreen?style=flat-square)](https://github.com/PookiePepelss/multiroblox/releases)

</div>

no installer needed. grab the exe from [releases](https://github.com/PookiePepelss/multiroblox/releases) and run it, or build from source:

```bash
git clone https://github.com/PookiePepelss/multiroblox.git
cd multiroblox
npm install && npm run build
```

## features

**accounts**
- launch as many accounts side by side as you want
- sign in through a real chrome window or paste your cookie directly
- set a game id or private server link per account so it launches straight there
- assign nicknames to accounts for easy identification
- filter and search across all saved accounts
- cookies are encrypted with AES-256-GCM and stored locally. nothing leaves your device

**packages**
- group accounts into packages (e.g. farm squad, trading alts)
- launch an entire group at once with a shared or per-account game target

**mixer**
- control render quality, fps target, and volume for all running instances from one panel
- graphics quality and fps settings write to the roblox fast flags on disk
- live volume control adjusts all running roblox sessions at the os level
- kill all roblox instances with one button

**charts**
- browse top playing now, top rated, and top earning games
- search and launch any game directly from the charts page

**generator**
- generate roblox accounts using a [bloxgen.net](https://bloxgen.net/) api key

**settings**
- general, themes, and sounds tabs
- custom encryption key with AES-256-GCM or OS-native DPAPI (windows keychain)
- custom ui sound profiles with volume control and upload-your-own support
- light/dark theme toggle

**anti-afk**
- toggle from the sidebar. taps a benign key into every open roblox window on a configurable interval so the idle kick never fires

**logs**
- real-time log viewer with in-page search (ctrl+f)

## how it works

roblox prevents multiple instances by holding a windows mutex. multiroblox grabs that mutex first through a lightweight native helper (`RobloxNative.exe`, written in c#) so roblox opens a fresh instance every time. each account gets its own auth ticket before launch so they all sign in as different accounts.

browser login downloads a private copy of chrome on first use and won't touch your existing install.

if the native helper isn't shipped with a build, it compiles from the bundled source using the .net framework `csc.exe` that's already on every windows machine.

## license

MIT
