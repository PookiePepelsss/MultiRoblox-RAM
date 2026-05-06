# MultiRoblox

run multiple roblox accounts at the same time on windows. built with electron.

![Platform](https://img.shields.io/badge/platform-Windows-blue) ![Version](https://img.shields.io/badge/version-1.0.0-green)

---

## what it does

- launch as many accounts as you want side by side
- sign in through a real chrome window or just paste your cookie
- set a game or private server link per account so it goes straight there on launch
- fast flag editor so you dont have to dig through appdata
- browse top games and launch into them directly
- account generator that cycles through a combo list you give it
- cookies are encrypted on disk so theyre not just sitting there in plaintext

## requirements

- windows 10 or 11
- roblox installed
- node.js + npm if youre building from source

## download

grab the exe from [releases](../../releases), its portable so no installer needed

## building from source

```bash
git clone https://github.com/PookiePepelss/multiroblox.git
cd multiroblox
npm install
npm run build
```

output goes to `dist/`. to just run it without building use `npm start`

## how it works

roblox blocks multiple instances by holding a windows mutex called `ROBLOX_singletonMutex`. the app spawns a powershell process that grabs that mutex before roblox does so roblox just opens a fresh instance every time. each account gets its own auth ticket fetched from roblox's api before launch so they all sign in as different accounts.

## adding accounts

hit **Add account** in the sidebar. you can either sign in through a chrome window the app opens, or paste your `.ROBLOSECURITY` cookie directly (`F12` → Application → Cookies → roblox.com).

## game targets

click the edit button on a card to set a game. supports plain place ids, full game urls, and private server links (both the old `privateServerLinkCode` format and the newer `roblox.com/share` ones). leave it blank to just open roblox normally.

## fast flags

edits `ClientAppSettings.json` directly. you can add flags one by one, paste a full json blob, or export what you have. applies on next roblox launch.

## generator

click Generate to pull the next one. 60 second cooldown between each.

## encryption

cookies are encrypted with AES-256-GCM before being saved. the app generates a device key on first launch, or you can set your own passphrase in settings if you want it to work across machines.

## notes

the mutex trick has worked for years and roblox hasnt changed it. if something breaks after an update open an issue.

browser login downloads a separate copy of chrome on first use so it doesnt touch your actual chrome install.

## license

MIT
