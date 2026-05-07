# MultiRoblox

[![Release](https://img.shields.io/github/v/release/PookiePepelss/multiroblox?style=flat-square&label=download&color=5c5ce0)](https://github.com/PookiePepelss/multiroblox/releases/latest)
![Platform](https://img.shields.io/badge/platform-windows-333?style=flat-square)
![Electron](https://img.shields.io/badge/built%20with-electron-47848f?style=flat-square)
![License](https://img.shields.io/badge/license-MIT-3ecf8e?style=flat-square)

run multiple roblox accounts at the same time on windows

grab the exe from [releases](../../releases), no installer needed. or build from source:

```bash
git clone https://github.com/PookiePepelss/multiroblox.git
cd multiroblox
npm install && npm run build
```

## features

- launch as many accounts side by side as you want
- sign in through a real chrome window or paste your cookie
- set a game or private server link per account so it launches straight there
- fast flag editor, charts, account generator, encrypted cookie storage

## how it works

roblox prevents multiple instances by holding a windows mutex. the app grabs that mutex first so roblox opens a fresh instance every time. each account gets its own auth ticket before launch so they all sign in as different accounts.

browser login downloads a copy of chrome on first use, won't touch your existing install. if something breaks after a roblox update open an issue.

## license

MIT
