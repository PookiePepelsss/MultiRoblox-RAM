// The full game page opened from Charts: stats, thumbnails, a Play button with
// an account picker, and the game's public and private servers.
const GV = {
  token: 0, game: null, details: null, votes: null, favorites: null, shots: [], shot: 0,
  accountId: null, launching: false,
  tab: 'public', sort: 'full', hideFull: true,
  servers: null, serversLoading: false, serversRequest: 0,
  priv: null, privError: null, privLoading: false,
  price: undefined, buying: false, buyOpen: false, buyKey: null, renamingId: null,
};

function gvCompact(n) {
  if (typeof n !== 'number' || !isFinite(n)) return '-';
  return new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 }).format(n);
}
function gvDate(s) {
  const d = new Date(s);
  return s && !isNaN(d) && d.getFullYear() > 2000 ? d.toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' }) : '-';
}
function gvAccount() { return accounts.find(a => a.id === GV.accountId) || null; }
function gvAccountName(a) { return a.nickname || a.username || 'Unknown'; }
function gvPickDefaultAccount() {
  const current = gvAccount();
  if (current && !_launchedIds.has(current.id)) return;
  const free = accounts.find(a => a.cookie && !_launchedIds.has(a.id) && _cookieStatus[a.id] !== 'dead');
  GV.accountId = (free || current || accounts[0] || { id: null }).id;
}
async function gvGet(url) {
  try {
    const r = await api.robloxGet(url);
    return r && r.ok ? r.data : null;
  } catch { return null; }
}

async function openGameView(g) {
  const token = ++GV.token;
  Object.assign(GV, {
    game: g, details: null, votes: null, favorites: null, shots: [], shot: 0,
    tab: 'public', servers: null,
    priv: null, privError: null, price: undefined, buyOpen: false, renamingId: null,
  });
  gvPickDefaultAccount();
  gvRender();
  const view = document.getElementById('game-view');
  view.scrollTop = 0;
  view.classList.add('open');

  const u = g.universeId;
  const [details, votes, favorites, thumbs] = await Promise.all([
    gvGet(`https://games.roblox.com/v1/games?universeIds=${u}`),
    gvGet(`https://games.roblox.com/v1/games/votes?universeIds=${u}`),
    gvGet(`https://games.roblox.com/v1/games/${u}/favorites/count`),
    gvGet(`https://thumbnails.roblox.com/v1/games/multiget/thumbnails?universeIds=${u}&countPerUniverse=10&defaults=true&size=768x432&format=Png&isCircular=false`),
  ]);
  if (token !== GV.token) return;
  GV.details = details?.data?.[0] || null;
  GV.votes = votes?.data?.[0] || null;
  GV.favorites = typeof favorites?.favoritesCount === 'number' ? favorites.favoritesCount : (GV.details?.favoritedCount ?? null);
  GV.shots = (thumbs?.data?.[0]?.thumbnails || []).filter(t => t.imageUrl && t.state === 'Completed').map(t => t.imageUrl);
  gvRender();
  gvLoadPublic();
}

function closeGameView() {
  GV.token++;
  document.getElementById('game-view')?.classList.remove('open');
}

function gvRender() {
  const g = GV.game;
  if (!g) return;
  const d = GV.details || {};
  const up = GV.votes?.upVotes, down = GV.votes?.downVotes;
  const total = (up || 0) + (down || 0);
  const pct = total ? Math.round((up / total) * 100) : null;
  const playing = typeof d.playing === 'number' ? d.playing : g.playerCount;
  // Carried over the rebuild below so a details update doesn't restart its fade.
  const glow = document.getElementById('gv-glow');
  const stat = (label, value) => `<div class="gv-stat"><div class="gv-stat-label">${label}</div><div class="gv-stat-value">${esc(value)}</div></div>`;
  document.getElementById('game-view').innerHTML = `
    <div class="gv-wrap">
      <button class="gv-back" onclick="closeGameView()"><span class="material-icons-round">arrow_back</span>Charts</button>
      <div class="gv-hero">
        <div class="gv-media" id="gv-media"></div>
        <div class="gv-info">
          <h1 class="gv-title">${esc(d.name || g.name || 'Unknown')}</h1>
          <div class="gv-by">By <strong>${esc(d.creator?.name || '-')}</strong>${d.creator?.hasVerifiedBadge ? '<span class="material-icons-round gv-verified">verified</span>' : ''}</div>
          <div class="gv-play" id="gv-play"></div>
          <div class="gv-votes">
            <div class="gv-vote-row">
              <span><span class="material-icons-round">thumb_up</span>${gvCompact(up)}</span>
              <strong>${pct === null ? '-' : pct + '%'}</strong>
              <span>${gvCompact(down)}<span class="material-icons-round">thumb_down</span></span>
            </div>
            <div class="gv-vote-bar"><div style="width:${pct === null ? 0 : pct}%"></div></div>
          </div>
          <div class="gv-links">
            <button class="btn btn-ghost" onclick="gvCopyPlace()"><span class="material-icons-round">content_copy</span>Copy ID</button>
            <button class="btn btn-ghost" onclick="gvOpenOnRoblox()"><span class="material-icons-round">open_in_new</span>Roblox</button>
          </div>
        </div>
      </div>
      <div class="gv-stats">
        ${stat('Active', gvCompact(playing))}
        ${stat('Favorites', gvCompact(GV.favorites))}
        ${stat('Visits', gvCompact(d.visits))}
        ${stat('Server size', d.maxPlayers ? String(d.maxPlayers) : '-')}
        ${stat('Genre', d.genre || '-')}
        ${stat('Created', gvDate(d.created))}
        ${stat('Updated', gvDate(d.updated))}
      </div>
      ${d.description ? `<section class="gv-section"><div class="gv-section-title">About</div><p class="gv-desc">${esc(d.description)}</p></section>` : ''}
      <section class="gv-section" id="gv-servers"></section>
    </div>`;
  document.querySelector('#game-view .gv-wrap').prepend(glow || gvNewGlow());
  gvRenderMedia();
  gvRenderPlay();
  gvRenderServers();
}

function gvNewGlow() {
  const glow = document.createElement('div');
  glow.className = 'gv-glow';
  glow.id = 'gv-glow';
  glow.innerHTML = '<canvas></canvas><canvas></canvas>';
  return glow;
}

// A blurred copy of the current picture sits behind the top of the page, so the
// page takes on that picture's colors. Two layers let one fade into the next.
function gvUpdateGlow() {
  const glow = document.getElementById('gv-glow');
  if (!glow) return;
  const url = gvShots()[GV.shot] || '';
  if (glow.dataset.url === url) return;
  glow.dataset.url = url;
  const [a, b] = glow.children;
  const shown = a.classList.contains('on') ? a : b, next = shown === a ? b : a;
  shown.classList.remove('on');
  if (!url) return;
  paintGlow(next, url, 70, 140, () => { if (glow.dataset.url === url) next.classList.add('on'); });
}

function gvShots() {
  return GV.shots.length ? GV.shots : (GV.game.thumbUrl ? [GV.game.thumbUrl] : []);
}

function gvRenderMedia() {
  const el = document.getElementById('gv-media');
  if (!el) return;
  const shots = gvShots();
  if (!shots.length) { el.innerHTML = '<div class="gv-shot gv-shot-empty"><span class="material-icons-round">videogame_asset</span></div>'; gvUpdateGlow(); return; }
  GV.shot = Math.min(GV.shot, shots.length - 1);
  gvUpdateGlow();
  shots.forEach(s => { new Image().src = s; }); // so the arrows never wait on a download
  el.innerHTML = `
    <div class="gv-shot">
      <img id="gv-shot-img" src="${esc(shots[GV.shot])}" alt=""/>
      ${shots.length > 1 ? `
        <button class="gv-arrow left" onclick="gvShot(GV.shot - 1)" aria-label="Previous"><span class="material-icons-round">chevron_left</span></button>
        <button class="gv-arrow right" onclick="gvShot(GV.shot + 1)" aria-label="Next"><span class="material-icons-round">chevron_right</span></button>` : ''}
    </div>
    ${shots.length > 1 ? `<div class="gv-strip" id="gv-strip">${shots.map((s, n) => `<button class="gv-thumb${n === GV.shot ? ' active' : ''}" onclick="gvShot(${n})"><img src="${esc(s)}" alt="" loading="lazy"/></button>`).join('')}</div>` : ''}`;
}

// Only the picture and highlight change. Rebuilding the area on each click
// removed the arrow from under the cursor, which is what made it jump.
function gvShot(n) {
  const shots = gvShots();
  if (!shots.length) return;
  GV.shot = (n + shots.length) % shots.length;
  gvUpdateGlow();
  const img = document.getElementById('gv-shot-img');
  if (img) {
    img.src = shots[GV.shot];
    img.classList.remove('fade');
    void img.offsetWidth;
    img.classList.add('fade');
  }
  const strip = document.getElementById('gv-strip');
  if (strip) {
    [...strip.children].forEach((t, i) => t.classList.toggle('active', i === GV.shot));
    const thumb = strip.children[GV.shot];
    strip.scrollTo({ left: thumb.offsetLeft - (strip.clientWidth - thumb.offsetWidth) / 2, behavior: 'smooth' });
  }
}

function gvRenderPlay() {
  const el = document.getElementById('gv-play');
  if (!el) return;
  const acc = gvAccount();
  const running = acc && _launchedIds.has(acc.id);
  const blocked = !acc || running || GV.launching;
  const label = GV.launching ? 'Starting' : !acc ? 'No accounts' : running ? 'Already running' : 'Play';
  const items = accounts.map(a => {
    const live = _launchedIds.has(a.id), dead = _cookieStatus[a.id] === 'dead';
    return `<button class="gv-acct${a.id === GV.accountId ? ' selected' : ''}" ${live || dead ? 'disabled' : ''} onclick="gvSelectAccount(${jsArg(a.id)})">
      <span class="gv-acct-av">${a.userId && _avatarCache[a.userId] ? `<img src="${esc(_avatarCache[a.userId])}" alt=""/>` : esc((a.username || '?')[0].toUpperCase())}</span>
      <span class="gv-acct-name">${esc(gvAccountName(a))}</span>
      ${a.tag ? `<span class="card-tag${a.tag === 'MAIN' ? ' main' : ''}">${esc(a.tag)}</span>` : ''}
      <span class="gv-acct-state">${live ? 'Running' : dead ? 'Expired' : ''}</span>
    </button>`;
  }).join('');
  el.innerHTML = `
    <button class="gv-play-main" ${blocked ? 'disabled' : ''} onclick="gvPlay()">
      ${GV.launching ? '<div class="spin"></div>' : '<span class="material-icons-round">play_arrow</span>'}${label}
    </button>
    <button class="gv-play-pick" ${accounts.length ? '' : 'disabled'} onclick="gvToggleAccounts(event)" title="Choose an account">
      <span class="gv-play-acct">${esc(acc ? gvAccountName(acc) : 'None')}</span>
      <span class="material-icons-round">expand_more</span>
    </button>
    <div class="gv-acct-menu" id="gv-acct-menu">${items}</div>`;
}
function gvToggleAccounts(e) {
  e.stopPropagation();
  const wasOpen = document.getElementById('gv-acct-menu')?.classList.contains('open');
  gvRenderPlay();
  if (wasOpen) return;
  const menu = document.getElementById('gv-acct-menu');
  void menu.offsetWidth; // the list was just rebuilt; let it start closed so it fades in
  menu.classList.add('open');
}
document.addEventListener('click', () => document.getElementById('gv-acct-menu')?.classList.remove('open'));
function gvSelectAccount(id) {
  GV.accountId = id;
  GV.priv = null; GV.privError = null; GV.buyOpen = false; GV.renamingId = null;
  gvRenderPlay();
  gvRenderServers();
  if (GV.tab === 'private') gvLoadPrivate();
}

async function gvLaunchInto(target, what) {
  const acc = gvAccount();
  if (!acc || GV.launching) return;
  if (_launchedIds.has(acc.id)) { toast(gvAccountName(acc) + ' is already running', 'err'); return; }
  GV.launching = true;
  gvRenderPlay();
  gvRenderServers();
  logEntry('info', 'launch', `Launching ${acc.username || acc.id} into ${what}...`, { accountId: acc.id });
  let res;
  try { res = await api.launchRoblox(acc.id, acc.cookie, target, false); }
  catch (e) { res = { success: false, error: e?.message || String(e) }; }
  GV.launching = false;
  if (res && res.success) {
    markLaunched(acc.id);
    toast('Launched ' + gvAccountName(acc) + ' into ' + what, 'ok');
    gvPickDefaultAccount();
  } else if (!res || !res.cancelled) {
    const why = (res && res.error) || 'Launch failed';
    logEntry('err', 'launch', `Launch failed for ${acc.username || acc.id}: ${why}`, { accountId: acc.id });
    _flagCookieMaybeDead(acc.id, why);
    toast(why, 'err');
  }
  gvRenderPlay();
  gvRenderServers();
}
function gvPlay() { gvLaunchInto(String(GV.game.placeId), GV.game.name || 'the game'); }
function gvCopyPlace() { navigator.clipboard.writeText(String(GV.game.placeId)).then(() => toast('Place ID copied', 'ok')); }
function gvOpenOnRoblox() { api.openExternal('https://www.roblox.com/games/' + GV.game.placeId); }

// ── Servers ─────────────────────────────────────────────────────────────────
// Asks Roblox for the servers this view is about. It only ever hands out 100 at
// a time, fullest first unless told otherwise, so sorting one fixed download
// couldn't show the emptiest servers or hide full ones on a busy game.
const GV_PING_PAGES = 3; // ping can't be requested, so look through more servers
// Signed in as the chosen account when there is one: only a signed-in read
// carries the players' tokens that the faces in each row come from.
async function gvServersFetch(url) {
  const acc = gvAccount();
  if (acc?.cookie) {
    try {
      const r = await api.publicServersAs(acc.cookie, url);
      if (r && r.ok && Array.isArray(r.data?.data)) return r.data;
    } catch {}
  }
  return gvGet(url);
}

// Player faces by token, kept for the session; '' marks one with no picture.
const _faceCache = {};
const GV_FACES = 4;
function gvFaces(s) {
  const tokens = (s.playerTokens || []).slice(0, GV_FACES);
  if (!tokens.length) return '';
  return `<span class="gv-faces">${tokens.map(t => `<img data-token="${esc(t)}" ${_faceCache[t] ? `src="${esc(_faceCache[t])}"` : ''} alt=""/>`).join('')}</span>`;
}
async function gvLoadFaces(list) {
  const want = [...new Set(list.flatMap(s => (s.playerTokens || []).slice(0, GV_FACES)))].filter(t => !(t in _faceCache));
  for (let i = 0; i < want.length; i += 100) {
    const chunk = want.slice(i, i + 100);
    let found;
    try { found = (await api.headshots(chunk)) || {}; } catch { return; }
    chunk.forEach(t => { _faceCache[t] = found[t] || ''; });
    document.querySelectorAll('#gv-servers img[data-token]:not([src])').forEach(img => {
      const url = _faceCache[img.dataset.token];
      if (url) img.src = url;
    });
  }
}

// Roblox rate-limits this list when it's asked often, so a failed read is tried
// again quietly. With nothing on screen it keeps trying, more slowly, for as long
// as the page is open; with a list showing it stops after a few tries and keeps it.
async function gvGetServers(url, stillWanted, keepTrying) {
  for (let tries = 1; ; tries++) {
    const data = await gvServersFetch(url);
    if (data && Array.isArray(data.data)) return data;
    if (!keepTrying && tries >= 3) return null;
    await new Promise(done => setTimeout(done, Math.min(1500 * tries, 10000)));
    if (!stillWanted()) return null;
  }
}

async function gvLoadPublic() {
  const token = GV.token, request = ++GV.serversRequest;
  GV.serversLoading = true;
  gvRenderServers();
  const base = `https://games.roblox.com/v1/games/${GV.game.placeId}/servers/Public`
    + `?sortOrder=${GV.sort === 'empty' ? 'Asc' : 'Desc'}&excludeFullGames=${GV.hideFull}&limit=100`;
  const list = [];
  const wanted = () => token === GV.token && request === GV.serversRequest;
  let answered = false, cursor = '';
  for (let page = 0; page < (GV.sort === 'ping' ? GV_PING_PAGES : 1); page++) {
    const url = base + (cursor ? '&cursor=' + encodeURIComponent(cursor) : '');
    const data = await gvGetServers(url, wanted, !page && !GV.servers?.length);
    if (!wanted()) return;
    if (!data) break;
    answered = true;
    list.push(...data.data);
    cursor = data.nextPageCursor;
    if (!cursor) break;
  }
  GV.serversLoading = false;
  if (answered) GV.servers = list;
  gvRenderServers();
  if (answered) gvLoadFaces(list);
}
async function gvLoadPrivate() {
  const acc = gvAccount();
  if (!acc) return;
  const token = GV.token, id = acc.id;
  GV.privLoading = true; GV.privError = null;
  gvRenderServers();
  try {
    const [list, price] = await Promise.all([
      api.serversList(id, String(GV.game.placeId)),
      GV.price === undefined ? api.serversPrice(String(GV.game.placeId)).catch(() => ({ price: null })) : Promise.resolve({ price: GV.price }),
    ]);
    if (token !== GV.token || id !== GV.accountId) return;
    GV.priv = list.servers || [];
    GV.price = price.price ?? null;
  } catch (e) {
    if (token !== GV.token || id !== GV.accountId) return;
    GV.privError = typeof e === 'string' ? e : (e.message || 'Could not load private servers.');
  }
  GV.privLoading = false;
  gvRenderServers();
}
function gvSetTab(tab) {
  GV.tab = tab;
  gvRenderServers();
  if (tab === 'private' && !GV.priv) gvLoadPrivate();
}
function gvSetSort(sort) {
  if (sort === GV.sort) return;
  GV.sort = sort;
  gvLoadPublic();
}
function gvToggleHideFull() { GV.hideFull = !GV.hideFull; gvLoadPublic(); }
function gvRefreshServers() {
  if (GV.tab !== 'public') gvLoadPrivate();
  else if (!GV.serversLoading) gvLoadPublic(); // repeat clicks are what trip the rate limit
}

function gvShortJob(id) { return String(id).slice(0, 8) + '…' + String(id).slice(-4); }
function gvCopy(text, what) { navigator.clipboard.writeText(text).then(() => toast(what + ' copied', 'ok')); }

function gvServerSkeleton() {
  const row = '<div class="gv-tr"><span class="skel skel-line"></span><span class="skel skel-line"></span><span class="skel skel-line"></span><span class="skel skel-line"></span><span class="skel skel-btn"></span></div>';
  return `<div class="gv-table"><div class="gv-tr gv-th"><span>Job ID</span><span>Players</span><span class="r">Ping</span><span class="r">FPS</span><span></span></div>${row.repeat(6)}</div>`;
}

function gvPublicRows() {
  if (!GV.servers || (GV.serversLoading && !GV.servers.length)) return gvServerSkeleton();
  let list = GV.servers.slice();
  if (GV.hideFull) list = list.filter(s => (s.playing || 0) < s.maxPlayers);
  if (GV.sort === 'full') list.sort((a, b) => (b.playing || 0) - (a.playing || 0));
  else if (GV.sort === 'empty') list.sort((a, b) => (a.playing || 0) - (b.playing || 0));
  else list.sort((a, b) => (a.ping ?? 1e9) - (b.ping ?? 1e9));
  if (!list.length) return '<div class="gv-note">No servers to show.</div>';
  const canJoin = gvAccount() && !_launchedIds.has(GV.accountId) && !GV.launching;
  const place = String(GV.game.placeId);
  return `<div class="gv-table${GV.serversLoading ? ' loading' : ''}">
    <div class="gv-tr gv-th"><span>Job ID</span><span>Players</span><span class="r">Ping</span><span class="r">FPS</span><span></span></div>
    ${list.map(s => {
      const pct = Math.round(((s.playing || 0) / (s.maxPlayers || 1)) * 100);
      return `<div class="gv-tr">
        <span class="gv-mono">${esc(gvShortJob(s.id))}</span>
        <span class="gv-players">${gvFaces(s)}<span class="gv-bar"><i style="width:${pct}%"></i></span><span class="gv-mono">${s.playing || 0}/${s.maxPlayers}</span></span>
        <span class="r gv-mono">${s.ping != null ? esc(s.ping) + ' ms' : '-'}</span>
        <span class="r gv-mono">${typeof s.fps === 'number' ? s.fps.toFixed(1) : '-'}</span>
        <span class="gv-row-actions">
          <button class="btn btn-ghost gv-icon" title="Copy job ID" onclick="gvCopy(${jsArg(String(s.id))},'Job ID')"><span class="material-icons-round">content_copy</span></button>
          <button class="btn btn-primary gv-join" ${canJoin ? '' : 'disabled'} onclick="gvLaunchInto(${jsArg(place + ':' + s.id)},'that server')">Join</button>
        </span>
      </div>`;
    }).join('')}
  </div>`;
}

function gvPriceText() {
  if (GV.price === null || GV.price === undefined) return 'Price unknown';
  return GV.price === 0 ? 'Free' : 'R$ ' + GV.price.toLocaleString();
}

function gvBuyPanel() {
  const allowed = GV.details?.createVipServersAllowed;
  if (allowed === false) return '<div class="gv-note">This game doesn\'t allow private servers.</div>';
  const acc = gvAccount();
  if (!acc) return '';
  if (!GV.buyOpen) {
    return `<button class="btn btn-ghost gv-buy-open" onclick="gvOpenBuy()"><span class="material-icons-round">add</span>Buy a private server<span class="gv-buy-price">${esc(gvPriceText())}</span></button>`;
  }
  const known = GV.price !== null && GV.price !== undefined;
  return `<div class="gv-buy">
    <input class="sr-input" id="gv-buy-name" maxlength="50" placeholder="Server name" value="${esc(gvAccountName(acc) + "'s server")}"/>
    ${known ? '' : '<input class="sr-input gv-buy-amount" id="gv-buy-amount" type="number" min="0" placeholder="Price in Robux"/>'}
    <button class="btn btn-ghost" onclick="gvCloseBuy()">Cancel</button>
    <button class="btn btn-primary" ${GV.buying ? 'disabled' : ''} onclick="gvBuy()">${GV.buying ? 'Buying' : known ? 'Buy for ' + esc(gvPriceText()) : 'Buy'}</button>
  </div>`;
}
function gvOpenBuy() { GV.buyOpen = true; GV.buyKey = crypto.randomUUID(); gvRenderServers(); }
function gvCloseBuy() { GV.buyOpen = false; gvRenderServers(); }

function gvBuy() {
  const acc = gvAccount();
  if (!acc || GV.buying) return;
  const name = (document.getElementById('gv-buy-name')?.value || '').trim();
  if (!name) { toast('Give the server a name', 'err'); return; }
  let price = GV.price;
  if (price === null || price === undefined) {
    price = parseInt(document.getElementById('gv-buy-amount')?.value, 10);
    if (!Number.isFinite(price) || price < 0) { toast('Enter the price in Robux', 'err'); return; }
  }
  const cost = price === 0 ? 'free' : 'R$ ' + price.toLocaleString() + ' per month';
  confirmAction(`Buy a private server for ${GV.game.name} on ${gvAccountName(acc)}? Cost: ${cost}.`, async () => {
    GV.buying = true;
    gvRenderServers();
    try {
      await api.serversBuy(acc.id, GV.game.universeId, name, price, GV.buyKey);
      GV.buyOpen = false;
      toast('Private server bought', 'ok');
      logEntry('ok', 'system', `Bought a private server for ${GV.game.name} on ${acc.username || acc.id}`, { accountId: acc.id, price });
      GV.buying = false;
      gvLoadPrivate();
      return;
    } catch (e) {
      const why = typeof e === 'string' ? e : (e.message || 'Purchase failed');
      // If Roblox couldn't be reached the purchase may still have gone through,
      // so a retry reuses this key and Roblox won't charge twice. Any answer
      // from Roblox means this attempt is over, so the next one is new.
      if (!why.startsWith('Could not reach')) GV.buyKey = crypto.randomUUID();
      toast(why, 'err');
      logEntry('err', 'system', `Private server purchase failed: ${why}`, { accountId: acc.id });
    }
    GV.buying = false;
    gvRenderServers();
  }, price === 0 ? 'Get it free' : 'Buy for R$ ' + price.toLocaleString());
}

async function gvMutate(work, done) {
  try {
    await work();
    if (done) toast(done, 'ok');
  } catch (e) {
    toast(typeof e === 'string' ? e : (e.message || 'That didn\'t work'), 'err');
  }
  gvLoadPrivate();
}
function gvStartRename(id) { GV.renamingId = id; gvRenderServers(); document.getElementById('gv-rename')?.focus(); }
function gvCancelRename() { GV.renamingId = null; gvRenderServers(); }
function gvSaveRename(id) {
  const name = (document.getElementById('gv-rename')?.value || '').trim();
  if (!name) return;
  GV.renamingId = null;
  gvMutate(() => api.serversUpdate(GV.accountId, id, { name }), 'Renamed');
}
function gvNewLink(id) {
  confirmAction('Make a new join link? The old link stops working.', () => gvMutate(() => api.serversUpdate(GV.accountId, id, { newJoinCode: true }), 'New link created'), 'Make new link');
}
function gvSetActive(id, active) {
  gvMutate(() => api.serversUpdate(GV.accountId, id, { active }), active ? 'Server turned on' : 'Server turned off');
}
function gvSetRenewal(id, active, price) {
  const apply = () => gvMutate(() => api.serversRenewal(GV.accountId, id, active, price), active ? 'Renewal on' : 'Renewal cancelled');
  if (active) confirmAction(`Turn monthly renewal back on? It will charge R$ ${Number(price).toLocaleString()} each month.`, apply, 'Turn renewal on');
  else apply();
}

function gvPrivateRow(s) {
  const sub = s.subscription || null;
  const parts = [];
  if (!s.owned && s.ownerName) parts.push('Owner: ' + s.ownerName);
  parts.push((s.playing || 0) + '/' + (s.maxPlayers || '?') + ' players');
  if (s.owned && s.active === false) parts.push('Turned off');
  if (sub && sub.expired) parts.push('Expired');
  else if (sub && sub.expirationDate) parts.push((sub.active ? 'Renews ' : 'Ends ') + gvDate(sub.expirationDate));
  const canJoin = gvAccount() && !_launchedIds.has(GV.accountId) && !GV.launching && s.accessCode && s.active !== false;
  const place = String(GV.game.placeId);
  const name = GV.renamingId === s.id
    ? `<div class="gv-rename"><input class="sr-input" id="gv-rename" maxlength="50" value="${esc(s.name || '')}" onkeydown="if(event.key==='Enter')gvSaveRename(${s.id});if(event.key==='Escape')gvCancelRename()"/><button class="btn btn-primary" onclick="gvSaveRename(${s.id})">Save</button></div>`
    : `<div class="gv-pv-name">${esc(s.name || 'Private server')}${s.owned ? '<span class="card-tag main">Yours</span>' : ''}</div>`;
  const icon = (title, glyph, call) => `<button class="btn btn-ghost gv-icon" title="${title}" onclick="${call}"><span class="material-icons-round">${glyph}</span></button>`;
  const tools = s.owned ? [
    s.link ? icon('Copy join link', 'link', `gvCopy(${esc(jsArg(s.link))},'Link')`) : '',
    icon('Rename', 'edit', `gvStartRename(${s.id})`),
    icon('New join link', 'autorenew', `gvNewLink(${s.id})`),
    sub && sub.price !== undefined ? icon(sub.active ? 'Cancel renewal' : 'Turn renewal on', sub.active ? 'event_busy' : 'event_repeat', `gvSetRenewal(${s.id},${!sub.active},${Number(sub.price) || 0})`) : '',
    icon(s.active === false ? 'Turn on' : 'Turn off', 'power_settings_new', `gvSetActive(${s.id},${s.active === false})`),
  ].join('') : '';
  return `<div class="gv-pv">
    <div class="gv-pv-main">${name}<div class="gv-pv-sub">${esc(parts.join(' · '))}</div></div>
    <div class="gv-row-actions">${tools}<button class="btn btn-primary gv-join" ${canJoin ? '' : 'disabled'} onclick="gvLaunchInto(${esc(jsArg('private:' + place + ':' + s.accessCode))},'a private server')">Join</button></div>
  </div>`;
}

function gvPrivateBody() {
  if (!gvAccount()) return '<div class="gv-note">Add an account to see private servers.</div>';
  if (GV.privError) return `<div class="gv-note">${esc(GV.privError)}</div>`;
  if (GV.privLoading && !GV.priv) return '<div class="gv-note"><div class="spin"></div>Loading private servers</div>';
  const rows = (GV.priv || []).map(gvPrivateRow).join('');
  return `${gvBuyPanel()}<div class="gv-pv-list">${rows || `<div class="gv-note">${esc(gvAccountName(gvAccount()))} has no private servers for this game.</div>`}</div>`;
}

function gvRenderServers() {
  const el = document.getElementById('gv-servers');
  if (!el) return;
  // Changing a selector rebuilds this section twice in a row (the choice, then
  // loading), so the old pills are carried into the new selectors and finish
  // the slide they're already on.
  const oldPills = {};
  el.querySelectorAll('.gv-seg > .seg-pill').forEach(pill => { oldPills[pill.parentElement.dataset.seg] = pill; });
  const seg = (key, active, items) => `<div class="gv-seg" data-seg="${key}">${items.map(([v, label, call]) => `<button class="${v === active ? 'active' : ''}" onclick="${call}">${label}</button>`).join('')}</div>`;
  const acc = gvAccount();
  el.innerHTML = `
    <div class="gv-server-head">
      <div class="gv-section-title">Servers</div>
      ${seg('gv-tab', GV.tab, [['public', 'Public', "gvSetTab('public')"], ['private', 'Private', "gvSetTab('private')"]])}
      ${GV.tab === 'public' ? seg('gv-sort', GV.sort, [['full', 'Most full', "gvSetSort('full')"], ['empty', 'Most empty', "gvSetSort('empty')"], ['ping', 'Best ping', "gvSetSort('ping')"]]) : `<span class="gv-for">As ${esc(acc ? gvAccountName(acc) : 'nobody')}</span>`}
      <span class="gv-spacer"></span>
      ${GV.tab === 'public' ? `<label class="strip-afk">Hide full<span class="toggle"><input type="checkbox" ${GV.hideFull ? 'checked' : ''} onchange="gvToggleHideFull()"/><span class="toggle-trk"></span></span></label>` : ''}
      <button class="btn btn-ghost" onclick="gvRefreshServers()"><span class="material-icons-round">refresh</span>Refresh</button>
    </div>
    ${GV.tab === 'public' ? gvPublicRows() : gvPrivateBody()}`;
  el.querySelectorAll('.gv-seg').forEach(s => {
    const pill = oldPills[s.dataset.seg];
    if (pill) s.prepend(pill);
    syncSeg(s, s.dataset.seg);
  });
}
