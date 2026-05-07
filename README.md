# MultiRoblox

run multiple roblox accounts at the same time on windows

## what it does

- launch as many accounts side by side as you want
- sign in through a real chrome window or paste your cookie
- set a game or private server link per account so it launches straight there
- fast flag editor, charts, account generator, encrypted cookie storage

## requirements

windows 10/11 and roblox installed. node.js + npm only if building from source.

## download

grab the exe from [releases](../../releases), no installer needed

## building from source

```bash
git clone https://github.com/PookiePepelss/multiroblox.git
cd multiroblox
npm install
npm run build
```

output goes to `dist/`, or just run `npm start` to skip building

## how it works

roblox prevents multiple instances by holding a windows mutex. the app grabs that mutex first so roblox opens a fresh instance every time. each account gets its own auth ticket before launch so they all sign in as different accounts.

## notes

browser login downloads a copy of chrome on first use, it wont touch your existing chrome install. if something breaks after a roblox update open an issue.

## license

MIT
