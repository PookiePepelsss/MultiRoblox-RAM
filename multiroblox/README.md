# MultiRoblox

run multiple roblox accounts at the same time on windows

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

---

## requirements

- windows 10 or 11
- roblox installed
- node.js + npm if youre building from source

---

## download

just grab the exe from [releases](../../releases), its portable so no installer or anything

---

## building from source

```bash
git clone https://github.com/PookiePepelss/multiroblox.git
cd multiroblox
npm install
npm run build
```

built exe goes to `dist/`. to run without building:

```bash
npm start
```

---

## how the multi instance stuff works

roblox blocks multiple instances by holding a windows mutex called `ROBLOX_singletonMutex`. the app spawns a powershell process that grabs that mutex before roblox does, so roblox thinks nothing is running and opens a new instance every time. each account gets its own auth ticket from roblox's api so they all log in as different accounts.

---

## adding accounts

hit **Add account** in the sidebar

- **Sign in with Roblox** — opens a chrome window, just log in like normal and the app grabs the cookie automatically
- **Paste Cookie** — `F12` → Application → Cookies → roblox.com → copy `.ROBLOSECURITY` → paste it in

---

## setting a game target

click the edit button on a card to set where that account launches. you can use:

- a place id (just the numbers from the url)
- a full game url
- a private server link, both the old `privateServerLinkCode` format and the newer `roblox.com/share` links work

leave it blank and it just opens roblox normally

---

## fast flags

fast flags page lets you edit `ClientAppSettings.json` directly. add flags one by one, import a full json blob, or export what you have. changes apply next time roblox launches.

---

## generator

add your accounts to `genaccounts.json` in `%AppData%\multiroblox` as `username:password` per entry. click Generate to get the next one in the list. theres a 60 second cooldown per generate.

---

## encryption

cookies get encrypted before hitting disk using AES-256-GCM by default. the app makes a random device key on first launch. if you want you can set a custom passphrase in settings instead. AES-256-CBC is also available if you need it for some reason.

---

## faq

**does it work after roblox updates?**
yeah, the mutex trick has stayed the same for years. if something breaks open an issue.

**are my cookies safe?**
they never leave your pc. stored encrypted in `%AppData%\multiroblox\accounts.json` and only decrypted in memory right before launching.

**why is it downloading chrome?**
first launch only, it downloads a separate copy just for the login window so it doesnt mess with your actual chrome.

**mac/linux?**
roblox doesnt run there so no

---

## license

MIT
