// Blue Marble Front — M1 Alpha Client
// Connects to SpacetimeDB, renders globe, handles multiplayer input.

// ── SpacetimeDB SDK (lightweight wrapper until generated bindings are available) ──
interface StdbRow {
  id: number | bigint;
  [key: string]: any;
}

class StdbTable<T extends StdbRow> {
  rows = new Map<string, T>();
  onInsertCb?: (row: T) => void;
  onUpdateCb?: (oldRow: T, newRow: T) => void;
  onDeleteCb?: (row: T) => void;

  onInsert(cb: (row: T) => void) { this.onInsertCb = cb; }
  onUpdate(cb: (oldRow: T, newRow: T) => void) { this.onUpdateCb = cb; }
  onDelete(cb: (row: T) => void) { this.onDeleteCb = cb; }

  insert(row: T) {
    const key = String(row.id);
    this.rows.set(key, row);
    this.onInsertCb?.(row);
  }
  update(row: T) {
    const key = String(row.id);
    const old = this.rows.get(key);
    this.rows.set(key, row);
    if (old) this.onUpdateCb?.(old, row);
    else this.onInsertCb?.(row);
  }
  delete(id: number | bigint) {
    const key = String(id);
    const old = this.rows.get(key);
    if (old) {
      this.rows.delete(key);
      this.onDeleteCb?.(old);
    }
  }
  iter(): T[] { return Array.from(this.rows.values()); }
  find(pred: (row: T) => boolean): T | undefined { return this.iter().find(pred); }
  filter(pred: (row: T) => boolean): T[] { return this.iter().filter(pred); }
}

class StdbReducerCaller {
  constructor(private ws: WebSocket, private name: string) {}
  call(reducer: string, args: any[]) {
    this.ws.send(JSON.stringify({ type: 'call', reducer: `${this.name}/${reducer}`, args }));
  }
}

class SpacetimeDBClient {
  ws!: WebSocket;
  db = {
    matches: new StdbTable<any>(),
    players: new StdbTable<any>(),
    tile_chunks: new StdbTable<any>(),
    attacks: new StdbTable<any>(),
    cities: new StdbTable<any>(),
    chat: new StdbTable<any>(),
  };
  reducers!: StdbReducerCaller;
  identity: string = '';
  token: string = '';
  private connectCb?: (identity: string, token: string) => void;
  private onRowCb?: (table: string, op: string, row: any) => void;

  constructor(private url: string, private moduleName: string) {}

  onConnect(cb: (identity: string, token: string) => void) { this.connectCb = cb; }

  connect() {
    this.ws = new WebSocket(this.url);
    this.reducers = new StdbReducerCaller(this.ws, this.moduleName);
    this.ws.onopen = () => {
      this.ws.send(JSON.stringify({ type: 'identity' }));
    };
    this.ws.onmessage = (ev) => {
      const msg = JSON.parse(ev.data);
      this.handleMessage(msg);
    };
  }

  private handleMessage(msg: any) {
    if (msg.type === 'identity') {
      this.identity = msg.identity;
      this.token = msg.token;
      this.connectCb?.(this.identity, this.token);
    } else if (msg.type === 'subscription') {
      for (const row of msg.rows || []) {
        this.upsertRow(msg.table, row);
      }
    } else if (msg.type === 'transaction') {
      for (const up of msg.updates || []) {
        if (up.op === 'insert') this.upsertRow(up.table, up.row);
        else if (up.op === 'update') this.upsertRow(up.table, up.row);
        else if (up.op === 'delete') this.deleteRow(up.table, up.row.id);
      }
    }
  }

  private upsertRow(table: string, row: any) {
    const t = (this.db as any)[table] as StdbTable<any>;
    if (!t) return;
    t.update(row);
    this.onRowCb?.(table, 'update', row);
  }

  private deleteRow(table: string, id: any) {
    const t = (this.db as any)[table] as StdbTable<any>;
    if (!t) return;
    t.delete(id);
  }

  subscribe(queries: string[]) {
    this.ws.send(JSON.stringify({ type: 'subscribe', queries }));
  }
}

// ── Constants ──────────────────────────────────────────────────────────────────
const SIM_W = 1350;
const SIM_H = 675;
const VISUAL_W = 5400;
const VISUAL_H = 2700;
const SCALE = VISUAL_W / SIM_W; // 4
const COLORS = ['#ee6633','#44cc88','#ff9900','#cc44ff','#00bbff','#ff4444','#88ff00','#ffaa00','#0088ff','#ff00cc','#00ffbb','#ffff44','#ff8844'];
const VISUAL_URL = 'https://assets.science.nasa.gov/content/dam/science/esd/eo/images/bmng/bmng-base/may/world.200405.3x5400x2700.jpg';
const PROXY = 'https://corsproxy.io/?';

// ── State ──────────────────────────────────────────────────────────────────────
let client: SpacetimeDBClient;
let myIdentity = '';
let myPlayerId = -1;
let currentMatchId = -1;
let currentPhase = 'Lobby';
let earthImg: HTMLImageElement | null = null;
let overlayCanvas: HTMLCanvasElement | null = null;
let overlayCtx: CanvasRenderingContext2D | null = null;
let overlayImageData: ImageData | null = null;
let view = { x: 0, y: 0, scale: 1 };
let gameStarted = false;
let gameOver = false;
let tickN = 0;
let totalLand = 0;
let chatOpen = false;

// ── DOM refs ───────────────────────────────────────────────────────────────────
const canvas = document.getElementById('canvas') as HTMLCanvasElement;
const ctx = canvas.getContext('2d')!;
const minimap = document.getElementById('minimap') as HTMLCanvasElement;
const mmCtx = minimap.getContext('2d')!;
const loadBar = document.getElementById('loadBar') as HTMLDivElement;
const loadMsg = document.getElementById('loadMsg') as HTMLDivElement;
const loading = document.getElementById('loading') as HTMLDivElement;
const startScreen = document.getElementById('startScreen') as HTMLDivElement;
const listScreen = document.getElementById('listScreen') as HTMLDivElement;
const lobbyScreen = document.getElementById('lobbyScreen') as HTMLDivElement;
const hud = document.getElementById('hud') as HTMLDivElement;
const bottomBar = document.getElementById('bottomBar') as HTMLDivElement;
const gameOverScreen = document.getElementById('gameOver') as HTMLDivElement;
const statusEl = document.getElementById('status') as HTMLDivElement;
const chatPanel = document.getElementById('chatPanel') as HTMLDivElement;
const chatMessages = document.getElementById('chatMessages') as HTMLDivElement;

function show(el: HTMLElement) { el.classList.remove('hidden'); }
function hide(el: HTMLElement) { el.classList.add('hidden'); }
function setLoad(pct: number, msg: string) { loadBar.style.width = pct + '%'; loadMsg.textContent = msg; }

// ── Image loader ───────────────────────────────────────────────────────────────
function loadImg(url: string): Promise<HTMLImageElement> {
  return new Promise((res, rej) => {
    const img = new Image();
    img.crossOrigin = 'anonymous';
    img.onload = () => res(img);
    img.onerror = () => {
      if (url.startsWith(PROXY)) { rej(new Error('proxy failed: ' + url)); return; }
      const img2 = new Image();
      img2.crossOrigin = 'anonymous';
      img2.onload = () => res(img2);
      img2.onerror = () => rej(new Error('failed: ' + url));
      img2.src = PROXY + encodeURIComponent(url);
    };
    img.src = url;
  });
}

// ── SpacetimeDB connection ─────────────────────────────────────────────────────
function connectStdb() {
  setLoad(10, 'Connecting to SpacetimeDB…');
  // Adjust URL to your SpacetimeDB host
  const wsUrl = (location.protocol === 'https:' ? 'wss:' : 'ws:') + '//' + location.host + '/v1/database/blue-marble-front/subscribe';
  client = new SpacetimeDBClient(wsUrl, 'blue-marble-front');
  client.onConnect((identity, token) => {
    myIdentity = identity;
    setLoad(30, 'Subscribing to tables…');
    client.subscribe([
      'SELECT * FROM matches',
      'SELECT * FROM players',
      'SELECT * FROM tile_chunks',
      'SELECT * FROM attacks',
      'SELECT * FROM cities',
      'SELECT * FROM chat',
    ]);
    setupDbListeners();
    setLoad(50, 'Loading NASA imagery…');
    loadNASAImages().then(() => {
      setLoad(100, 'Ready!');
      setTimeout(() => { hide(loading); show(startScreen); }, 400);
    }).catch(err => {
      loadMsg.textContent = '⚠️ Failed to load NASA imagery: ' + err.message;
      loadBar.style.background = '#f44';
    });
  });
  client.connect();
}

async function loadNASAImages() {
  earthImg = await loadImg(VISUAL_URL);
  overlayCanvas = document.createElement('canvas');
  overlayCanvas.width = VISUAL_W;
  overlayCanvas.height = VISUAL_H;
  overlayCtx = overlayCanvas.getContext('2d')!;
  overlayImageData = overlayCtx.createImageData(VISUAL_W, VISUAL_H);
}

// ── DB listeners ───────────────────────────────────────────────────────────────
function setupDbListeners() {
  client.db.matches.onUpdate((oldRow, newRow) => {
    if (Number(newRow.id) === currentMatchId) {
      currentPhase = newRow.phase;
      tickN = Number(newRow.tick);
      totalLand = Number(newRow.total_land);
      document.getElementById('hudTick')!.textContent = String(tickN);
      if (newRow.phase === 'Ended' && oldRow.phase !== 'Ended') {
        endGame(newRow.winner === myPlayerId);
      }
      if (newRow.phase === 'Playing' && oldRow.phase === 'Spawn') {
        statusEl.textContent = 'Game started! Click enemy territory to attack.';
      }
    }
    updateMatchList();
  });
  client.db.matches.onInsert(() => updateMatchList());

  client.db.players.onUpdate((_, p) => {
    if (Number(p.match_id) !== currentMatchId) return;
    if (p.identity === myIdentity && !p.is_bot) {
      myPlayerId = Number(p.id);
      updateHUD(p);
    }
    updateLobby();
  });
  client.db.players.onInsert((p) => {
    if (Number(p.match_id) !== currentMatchId) return;
    if (p.identity === myIdentity && !p.is_bot) myPlayerId = Number(p.id);
    updateLobby();
  });
  client.db.players.onDelete(() => updateLobby());

  client.db.tile_chunks.onUpdate(() => requestOverlayUpdate());
  client.db.tile_chunks.onInsert(() => requestOverlayUpdate());

  client.db.chat.onInsert((c) => {
    if (Number(c.match_id) !== currentMatchId) return;
    const div = document.createElement('div');
    div.className = 'chat-msg';
    const from = client.db.players.find(p => Number(p.id) === Number(c.from))?.name || '?';
    div.innerHTML = `<span class="chat-from">${escapeHtml(from)}:</span> ${escapeHtml(c.text)}`;
    chatMessages.appendChild(div);
    chatMessages.scrollTop = chatMessages.scrollHeight;
  });
}

function escapeHtml(s: string) {
  const d = document.createElement('div');
  d.textContent = s;
  return d.innerHTML;
}

// ── Overlay rendering from chunks ──────────────────────────────────────────────
let overlayDirty = false;
function requestOverlayUpdate() {
  if (overlayDirty) return;
  overlayDirty = true;
  requestAnimationFrame(() => {
    overlayDirty = false;
    buildOverlay();
  });
}

function buildOverlay() {
  if (!overlayImageData) return;
  const d = overlayImageData.data;
  d.fill(0);
  const chunks = client.db.tile_chunks.filter(c => Number(c.match_id) === currentMatchId);
  for (const chunk of chunks) {
    const cx = Number(chunk.chunk_x);
    const cy = Number(chunk.chunk_y);
    const owners: number[] = chunk.owners;
    for (let ly = 0; ly < 32; ly++) {
      for (let lx = 0; lx < 32; lx++) {
        const tx = cx * 32 + lx;
        const ty = cy * 32 + ly;
        if (tx >= SIM_W || ty >= SIM_H) continue;
        const ownerIdx = owners[ly * 32 + lx];
        if (ownerIdx === 255) continue;
        const color = hex2rgb(COLORS[ownerIdx % COLORS.length]);
        // Upscale 4x to visual resolution
        const vx = tx * SCALE;
        const vy = ty * SCALE;
        for (let dy = 0; dy < SCALE; dy++) {
          for (let dx = 0; dx < SCALE; dx++) {
            const px = vx + dx;
            const py = vy + dy;
            if (px >= VISUAL_W || py >= VISUAL_H) continue;
            const idx = (py * VISUAL_W + px) * 4;
            d[idx] = color.r;
            d[idx + 1] = color.g;
            d[idx + 2] = color.b;
            d[idx + 3] = 180;
          }
        }
      }
    }
  }
  overlayCtx?.putImageData(overlayImageData, 0, 0);
}

function hex2rgb(h: string) {
  const n = parseInt(h.slice(1), 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

// ── Render loop ────────────────────────────────────────────────────────────────
function render() {
  ctx.fillStyle = '#000810';
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  if (!earthImg) { requestAnimationFrame(render); return; }
  const { x, y, scale } = view;
  const dw = VISUAL_W * scale;
  const dh = VISUAL_H * scale;
  ctx.drawImage(earthImg, x, y, dw, dh);
  if (overlayCanvas && gameStarted) ctx.drawImage(overlayCanvas, x, y, dw, dh);
  requestAnimationFrame(render);
}

// ── HUD ────────────────────────────────────────────────────────────────────────
function updateHUD(p: any) {
  if (!p) return;
  const t = Number(p.tiles);
  const tr = Math.floor(Number(p.troops));
  const g = Math.floor(Number(p.gold));
  const mx = Number(p.max_troops);
  const pct = totalLand > 0 ? (t / totalLand * 100).toFixed(1) : '0.0';
  const color = COLORS[(p.color || 0) % COLORS.length];
  document.getElementById('hudNation')!.textContent = 'You (' + color + ')';
  (document.getElementById('hudNation') as HTMLElement).style.color = color;
  document.getElementById('hudTiles')!.textContent = t.toLocaleString();
  document.getElementById('hudTilesBar')!.style.width = Math.min(t / totalLand * 100 * 5, 100) + '%';
  document.getElementById('hudTroops')!.textContent = tr.toLocaleString();
  document.getElementById('hudTroopsBar')!.style.width = Math.min(tr / mx * 100, 100) + '%';
  document.getElementById('hudGold')!.textContent = g.toLocaleString();
  document.getElementById('hudPct')!.textContent = pct + '%';
  document.getElementById('hudTick')!.textContent = String(tickN);
  const alive = client.db.players.filter(p2 => Number(p2.match_id) === currentMatchId && p2.alive).length;
  statusEl.textContent = `Players alive: ${alive} | Your tiles: ${t.toLocaleString()} (${pct}%) | Gold: ${g.toLocaleString()}`;
}

// ── Minimap ────────────────────────────────────────────────────────────────────
function updateMinimap() {
  if (!earthImg || !overlayCanvas) return;
  mmCtx.drawImage(earthImg, 0, 0, 270, 135);
  mmCtx.drawImage(overlayCanvas, 0, 0, 270, 135);
  const scx = 270 / VISUAL_W;
  const scy = 135 / VISUAL_H;
  const vx = -view.x / view.scale * scx;
  const vy = -view.y / view.scale * scy;
  const vw = canvas.width / view.scale * scx;
  const vh = canvas.height / view.scale * scy;
  mmCtx.strokeStyle = 'rgba(255,255,255,0.7)';
  mmCtx.lineWidth = 1;
  mmCtx.strokeRect(vx, vy, vw, vh);
}
setInterval(updateMinimap, 500);

// ── View helpers ───────────────────────────────────────────────────────────────
function clampView() {
  const dw = VISUAL_W * view.scale;
  const dh = VISUAL_H * view.scale;
  const W = canvas.width;
  const H = canvas.height;
  if (dw <= W) view.x = (W - dw) / 2;
  else view.x = Math.min(0, Math.max(W - dw, view.x));
  if (dh <= H) view.y = (H - dh) / 2;
  else view.y = Math.min(0, Math.max(H - dh, view.y));
}

function fitView() {
  const scX = canvas.width / VISUAL_W;
  const scY = canvas.height / VISUAL_H;
  view.scale = Math.min(scX, scY);
  view.x = (canvas.width - VISUAL_W * view.scale) / 2;
  view.y = (canvas.height - VISUAL_H * view.scale) / 2;
}

// ── Input ──────────────────────────────────────────────────────────────────────
let drag: { sx: number; sy: number } | null = null;
let lastPinch = 0;

canvas.addEventListener('mousedown', e => { drag = { sx: e.clientX - view.x, sy: e.clientY - view.y }; });
window.addEventListener('mousemove', e => {
  if (!drag) return;
  view.x = e.clientX - drag.sx;
  view.y = e.clientY - drag.sy;
  clampView();
});
window.addEventListener('mouseup', () => { drag = null; });

canvas.addEventListener('wheel', e => {
  e.preventDefault();
  const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
  const cx = e.clientX;
  const cy = e.clientY;
  const oldScale = view.scale;
  view.scale = Math.max(0.12, Math.min(view.scale * factor, 12));
  const ef = view.scale / oldScale;
  view.x = (view.x - cx) * ef + cx;
  view.y = (view.y - cy) * ef + cy;
  clampView();
}, { passive: false });

canvas.addEventListener('touchstart', e => {
  if (e.touches.length === 1) drag = { sx: e.touches[0].clientX - view.x, sy: e.touches[0].clientY - view.y };
  if (e.touches.length === 2) {
    const dx = e.touches[0].clientX - e.touches[1].clientX;
    const dy = e.touches[0].clientY - e.touches[1].clientY;
    lastPinch = Math.sqrt(dx * dx + dy * dy);
  }
  e.preventDefault();
}, { passive: false });
canvas.addEventListener('touchmove', e => {
  if (e.touches.length === 1 && drag) {
    view.x = e.touches[0].clientX - drag.sx;
    view.y = e.touches[0].clientY - drag.sy;
    clampView();
  }
  if (e.touches.length === 2) {
    const dx = e.touches[0].clientX - e.touches[1].clientX;
    const dy = e.touches[0].clientY - e.touches[1].clientY;
    const dist = Math.sqrt(dx * dx + dy * dy);
    const factor = dist / lastPinch;
    const cx = (e.touches[0].clientX + e.touches[1].clientX) / 2;
    const cy = (e.touches[0].clientY + e.touches[1].clientY) / 2;
    const oldScale = view.scale;
    view.scale = Math.max(0.12, Math.min(view.scale * factor, 12));
    const ef = view.scale / oldScale;
    view.x = (view.x - cx) * ef + cx;
    view.y = (view.y - cy) * ef + cy;
    lastPinch = dist;
    clampView();
  }
  e.preventDefault();
}, { passive: false });
canvas.addEventListener('touchend', () => { drag = null; });

// Click handler: spawn / attack / inspect
canvas.addEventListener('click', e => {
  if (!gameStarted || gameOver) return;
  if (drag && (Math.abs(e.clientX - drag.sx) > 5 || Math.abs(e.clientY - drag.sy) > 5)) return;
  const worldX = (e.clientX - view.x) / view.scale;
  const worldY = (e.clientY - view.y) / view.scale;
  const gx = Math.floor(worldX / SCALE);
  const gy = Math.floor(worldY / SCALE);
  if (gx < 0 || gx >= SIM_W || gy < 0 || gy >= SIM_H) return;
  const tile = gy * SIM_W + gx;

  if (currentPhase === 'Spawn' && myPlayerId >= 0) {
    const me = client.db.players.find(p => Number(p.id) === myPlayerId);
    if (me && !me.spawn_tile) {
      client.reducers.call('spawn', [currentMatchId, tile]);
      statusEl.textContent = 'Spawn request sent…';
    }
  } else if (currentPhase === 'Playing') {
    const owner = getOwnerFromChunks(gx, gy);
    const me = client.db.players.find(p => Number(p.id) === myPlayerId);
    if (!me || !me.alive) return;
    if (owner === null || owner === 255) {
      statusEl.textContent = '⚠️ That is ocean or unclaimed.';
    } else if (owner === myPlayerId) {
      statusEl.textContent = 'Your territory. Use Build City to place a city here.';
    } else {
      // Attack
      client.reducers.call('launch_attack', [currentMatchId, owner]);
      statusEl.textContent = 'Attack launched!';
    }
  }
});

function getOwnerFromChunks(tx: number, ty: number): number | null {
  const cx = Math.floor(tx / 32);
  const cy = Math.floor(ty / 32);
  const lx = tx % 32;
  const ly = ty % 32;
  const chunk = client.db.tile_chunks.find(c =>
    Number(c.match_id) === currentMatchId &&
    Number(c.chunk_x) === cx &&
    Number(c.chunk_y) === cy
  );
  if (!chunk) return null;
  const idx = ly * 32 + lx;
  const val = chunk.owners[idx];
  return val === 255 ? null : val;
}

// ── UI Actions ─────────────────────────────────────────────────────────────────
document.getElementById('btnCreate')!.addEventListener('click', () => {
  const name = 'Match ' + Math.floor(Math.random() * 10000);
  client.reducers.call('create_match', [name]);
  // Wait for match to appear then join
  const check = setInterval(() => {
    const match = client.db.matches.find(m => m.name === name);
    if (match) {
      clearInterval(check);
      currentMatchId = Number(match.id);
      client.reducers.call('join_match', [currentMatchId, 'Player']);
      hide(startScreen);
      show(lobbyScreen);
      updateLobby();
    }
  }, 200);
});

document.getElementById('btnList')!.addEventListener('click', () => {
  hide(startScreen);
  show(listScreen);
  updateMatchList();
});

document.getElementById('btnBackFromList')!.addEventListener('click', () => {
  hide(listScreen);
  show(startScreen);
});

document.getElementById('btnStart')!.addEventListener('click', () => {
  if (currentMatchId >= 0) {
    client.reducers.call('start_match', [currentMatchId]);
  }
});

document.getElementById('btnAddBot')!.addEventListener('click', () => {
  if (currentMatchId >= 0) {
    const botNames = ['Alpha', 'Bravo', 'Charlie', 'Delta', 'Echo', 'Foxtrot'];
    const name = botNames[Math.floor(Math.random() * botNames.length)] + ' ' + Math.floor(Math.random() * 99);
    client.reducers.call('add_bot', [currentMatchId, name]);
  }
});

document.getElementById('btnLeave')!.addEventListener('click', () => {
  currentMatchId = -1;
  myPlayerId = -1;
  hide(lobbyScreen);
  hide(hud);
  hide(bottomBar);
  show(startScreen);
});

document.getElementById('btnBuildCity')!.addEventListener('click', () => {
  if (currentPhase !== 'Playing' || myPlayerId < 0) return;
  // For simplicity, build city on a random owned tile
  const me = client.db.players.find(p => Number(p.id) === myPlayerId);
  if (!me || !me.spawn_tile) return;
  const tile = Number(me.spawn_tile);
  client.reducers.call('build_city', [currentMatchId, tile]);
  statusEl.textContent = 'City build request sent.';
});

document.getElementById('btnRetreat')!.addEventListener('click', () => {
  if (currentPhase !== 'Playing' || myPlayerId < 0) return;
  const attacks = client.db.attacks.filter(a => Number(a.match_id) === currentMatchId && Number(a.attacker) === myPlayerId);
  for (const a of attacks) {
    client.reducers.call('retreat_attack', [currentMatchId, Number(a.target)]);
  }
  statusEl.textContent = 'Retreating all attacks.';
});

document.getElementById('btnChat')!.addEventListener('click', () => {
  chatOpen = !chatOpen;
  chatPanel.classList.toggle('hidden', !chatOpen);
});

document.getElementById('chatSend')!.addEventListener('click', sendChat);
document.getElementById('chatInput')!.addEventListener('keydown', e => {
  if (e.key === 'Enter') sendChat();
});
function sendChat() {
  const input = document.getElementById('chatInput') as HTMLInputElement;
  const text = input.value.trim();
  if (!text || currentMatchId < 0) return;
  client.reducers.call('send_chat', [currentMatchId, text]);
  input.value = '';
}

document.getElementById('replayBtn')!.addEventListener('click', () => {
  location.reload();
});

// ── Lobby & Match List UI ──────────────────────────────────────────────────────
function updateLobby() {
  const container = document.getElementById('lobbyPlayers')!;
  container.innerHTML = '';
  const players = client.db.players.filter(p => Number(p.match_id) === currentMatchId);
  for (const p of players) {
    const div = document.createElement('div');
    div.className = 'lobby-player';
    const color = COLORS[(p.color || 0) % COLORS.length];
    div.innerHTML = `<div class="dot" style="background:${color}"></div><div class="name">${escapeHtml(p.name)}</div><div class="tag">${p.is_bot ? 'BOT' : 'HUMAN'}</div>`;
    container.appendChild(div);
  }
  // Auto-transition to game when phase changes
  if (currentPhase === 'Playing' || currentPhase === 'Spawn') {
    hide(lobbyScreen);
    show(hud);
    show(bottomBar);
    gameStarted = true;
    fitView();
  }
}

function updateMatchList() {
  const container = document.getElementById('matchList')!;
  container.innerHTML = '';
  const matches = client.db.matches.iter().filter(m => m.phase === 'Lobby');
  for (const m of matches) {
    const row = document.createElement('div');
    row.className = 'match-row';
    const count = client.db.players.filter(p => Number(p.match_id) === Number(m.id)).length;
    row.innerHTML = `<div class="mname">${escapeHtml(m.name)}</div><div class="mstatus">${count}/8 players</div>`;
    row.addEventListener('click', () => {
      currentMatchId = Number(m.id);
      client.reducers.call('join_match', [currentMatchId, 'Player']);
      hide(listScreen);
      show(lobbyScreen);
      updateLobby();
    });
    container.appendChild(row);
  }
}

// ── End game ───────────────────────────────────────────────────────────────────
function endGame(won: boolean) {
  gameOver = true;
  const title = document.getElementById('goTitle')!;
  const detail = document.getElementById('goDetail')!;
  const statsEl = document.getElementById('goStats')!;
  title.textContent = won ? '🏆 Victory!' : '💀 Defeated';
  title.style.color = won ? '#4f8' : '#f44';
  const me = client.db.players.find(p => Number(p.id) === myPlayerId);
  const t = me ? Number(me.tiles) : 0;
  const pct = totalLand > 0 ? (t / totalLand * 100).toFixed(1) : '0.0';
  detail.textContent = won
    ? `You conquered ${pct}% of the planet in ${tickN} ticks!`
    : `Your nation fell after ${tickN} ticks with ${t.toLocaleString()} tiles.`;
  const alive = client.db.players.filter(p => Number(p.match_id) === currentMatchId && p.alive).length;
  statsEl.innerHTML = `
    <div class="stat-item"><div class="label">Ticks</div><div class="val">${tickN}</div></div>
    <div class="stat-item"><div class="label">Tiles held</div><div class="val">${t.toLocaleString()}</div></div>
    <div class="stat-item"><div class="label">Control</div><div class="val">${pct}%</div></div>
    <div class="stat-item"><div class="label">Survivors</div><div class="val">${alive}</div></div>`;
  hide(hud);
  hide(bottomBar);
  hide(chatPanel);
  show(gameOverScreen);
}

// ── Boot ───────────────────────────────────────────────────────────────────────
function resizeCanvas() {
  canvas.width = window.innerWidth;
  canvas.height = window.innerHeight;
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = 'high';
  if (earthImg) clampView();
}
window.addEventListener('resize', resizeCanvas);

resizeCanvas();
connectStdb();
render();
