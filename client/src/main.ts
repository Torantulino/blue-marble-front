// Blue Marble Front — M1 Alpha Client
// Connects to SpacetimeDB, renders globe, handles multiplayer input.

import { DbConnection, tables, reducers } from './generated/index';

// ── Constants ──────────────────────────────────────────────────────────────────
const SIM_W = 1350;
const SIM_H = 675;
const VISUAL_W = 5400;
const VISUAL_H = 2700;
const SCALE = VISUAL_W / SIM_W; // 4
const CHUNK_SIZE = 32;
const COLORS = ['#ee6633','#44cc88','#ff9900','#cc44ff','#00bbff','#ff4444','#88ff00','#ffaa00','#0088ff','#ff00cc','#00ffbb','#ffff44','#ff8844'];
const VISUAL_URL = 'https://assets.science.nasa.gov/content/dam/science/esd/eo/images/bmng/bmng-base/may/world.200405.3x5400x2700.jpg';
const PROXY = 'https://corsproxy.io/?';

// Use maincloud for production; allow override via env for local dev
const SPACETIMEDB_HOST = (import.meta as any).env?.VITE_SPACETIMEDB_HOST || 'wss://maincloud.spacetimedb.com';
const MODULE_NAME = 'blue-marble-front';

const PHASE_LOBBY = 0;
const PHASE_SPAWN = 1;
const PHASE_PLAYING = 2;
const PHASE_ENDED = 3;

// ── State ──────────────────────────────────────────────────────────────────────
let conn: DbConnection;
let myIdentityHex = '';
let myPlayerId = -1;
let currentMatchId = -1;
let currentPhase = PHASE_LOBBY;
let earthImg: HTMLImageElement | null = null;
let overlayCanvas: HTMLCanvasElement | null = null;
let overlayCtx: CanvasRenderingContext2D | null = null;
let overlayImageData: ImageData | null = null;
let view = { x: 0, y: 0, scale: 1 };
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
  conn = DbConnection.builder()
    .withUri(SPACETIMEDB_HOST)
    .withDatabaseName(MODULE_NAME)
    .onConnect((connection: any, identity: any, token: any) => {
      myIdentityHex = identity.toHexString();
      setLoad(30, 'Subscribing to tables…');
      connection.subscriptionBuilder()
        .onApplied(() => {
          setLoad(50, 'Loading NASA imagery…');
          loadNASAImages().then(() => {
            setLoad(100, 'Ready!');
            setTimeout(() => { hide(loading); show(startScreen); }, 400);
          }).catch((err: any) => {
            loadMsg.textContent = '⚠️ Failed to load NASA imagery: ' + (err?.message || err);
            loadBar.style.background = '#f44';
          });
        })
        .subscribe(
          tables.matches,
          tables.players,
          tables.tile_chunks,
          tables.attacks,
          tables.cities,
          tables.chat
        );
      setupDbListeners();
    })
    .onConnectError((ctx: any, error: any) => {
      loadMsg.textContent = '⚠️ Connection error: ' + error.message;
      loadBar.style.background = '#f44';
    })
    .build();
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
  conn.db.matches.onUpdate((_ctx, oldRow, newRow) => {
    if (Number(newRow.id) === currentMatchId) {
      currentPhase = newRow.phase;
      tickN = Number(newRow.tick);
      totalLand = Number(newRow.totalLand);
      const hudTick = document.getElementById('hudTick');
      if (hudTick) hudTick.textContent = String(tickN);
      if (newRow.phase === PHASE_ENDED && oldRow.phase !== PHASE_ENDED) {
        endGame(newRow.winner === myPlayerId);
      }
      if (newRow.phase === PHASE_PLAYING && oldRow.phase === PHASE_SPAWN) {
        statusEl.textContent = 'Game started! Click enemy territory to attack.';
      }
    }
  });

  conn.db.players.onInsert((_ctx, row) => {
    if (currentMatchId !== -1 && Number(row.matchId) === currentMatchId) {
      updateLobbyPlayers();
    }
    if (row.identity.toHexString() === myIdentityHex && !row.isBot) {
      myPlayerId = Number(row.id);
      const hudNation = document.getElementById('hudNation');
      if (hudNation) hudNation.textContent = row.name;
    }
  });

  conn.db.players.onUpdate((_ctx, _oldRow, newRow) => {
    if (Number(newRow.id) === myPlayerId) {
      const hudTiles = document.getElementById('hudTiles');
      const hudTroops = document.getElementById('hudTroops');
      const hudGold = document.getElementById('hudGold');
      const hudPct = document.getElementById('hudPct');
      const hudTilesBar = document.getElementById('hudTilesBar');
      const hudTroopsBar = document.getElementById('hudTroopsBar');
      if (hudTiles) hudTiles.textContent = String(newRow.tiles);
      if (hudTroops) hudTroops.textContent = String(Math.floor(newRow.troops));
      if (hudGold) hudGold.textContent = String(Math.floor(newRow.gold));
      const pct = totalLand > 0 ? Math.floor((newRow.tiles / totalLand) * 100) : 0;
      if (hudPct) hudPct.textContent = pct + '%';
      if (hudTilesBar) hudTilesBar.style.width = pct + '%';
      const troopPct = newRow.maxTroops > 0 ? Math.floor((newRow.troops / newRow.maxTroops) * 100) : 0;
      if (hudTroopsBar) hudTroopsBar.style.width = troopPct + '%';
    }
    if (currentMatchId !== -1 && Number(newRow.matchId) === currentMatchId) {
      updateLobbyPlayers();
    }
  });

  conn.db.chat.onInsert((_ctx, row) => {
    if (Number(row.matchId) === currentMatchId) {
      const div = document.createElement('div');
      div.className = 'chat-msg';
      const fromName = getPlayerName(Number(row.from));
      div.innerHTML = `<span class="chat-from">${escapeHtml(fromName)}:</span> ${escapeHtml(row.text)}`;
      chatMessages.appendChild(div);
      chatMessages.scrollTop = chatMessages.scrollHeight;
    }
  });

  conn.db.attacks.onInsert((_ctx, row) => {
    if (Number(row.attacker) === myPlayerId) {
      statusEl.textContent = 'Attacking ' + getPlayerName(Number(row.target)) + '!';
    }
  });

  conn.db.attacks.onDelete((_ctx, row) => {
    if (Number(row.attacker) === myPlayerId) {
      statusEl.textContent = 'Attack ended.';
    }
  });
}

function getPlayerName(pid: number): string {
  for (const p of conn.db.players.iter()) {
    if (Number(p.id) === pid) return p.name;
  }
  return '?';
}

function escapeHtml(s: string): string {
  const d = document.createElement('div');
  d.textContent = s;
  return d.innerHTML;
}

// ── UI event handlers ──────────────────────────────────────────────────────────
document.getElementById('btnCreate')!.addEventListener('click', () => {
  const name = 'Match ' + Math.floor(Math.random() * 9999);
  conn.reducers.createMatch({ name });
  const check = setInterval(() => {
    if (!myIdentityHex) return;
    for (const m of conn.db.matches.iter()) {
      if (m.creator.toHexString() === myIdentityHex) {
        clearInterval(check);
        currentMatchId = Number(m.id);
        enterLobby();
        break;
      }
    }
  }, 200);
  setTimeout(() => clearInterval(check), 5000);
});

document.getElementById('btnList')!.addEventListener('click', () => {
  hide(startScreen);
  show(listScreen);
  renderMatchList();
});

document.getElementById('btnBackFromList')!.addEventListener('click', () => {
  hide(listScreen);
  show(startScreen);
});

document.getElementById('btnStart')!.addEventListener('click', () => {
  if (currentMatchId !== -1) {
    conn.reducers.startMatch({ matchId: BigInt(currentMatchId) });
  }
});

document.getElementById('btnAddBot')!.addEventListener('click', () => {
  if (currentMatchId !== -1) {
    const names = ['AlphaBot', 'BetaBot', 'GammaBot', 'DeltaBot', 'EpsilonBot'];
    const botName = names[Math.floor(Math.random() * names.length)];
    conn.reducers.addBot({ matchId: BigInt(currentMatchId), botName });
  }
});

document.getElementById('btnLeave')!.addEventListener('click', () => {
  currentMatchId = -1;
  myPlayerId = -1;
  hide(lobbyScreen);
  show(startScreen);
});

document.getElementById('btnBuildCity')!.addEventListener('click', () => {
  statusEl.textContent = 'Click one of your tiles to build a city.';
  if (currentMatchId !== -1 && myPlayerId !== -1) {
    const me = Array.from(conn.db.players.iter()).find(p => Number(p.id) === myPlayerId);
    if (me && me.spawnTile !== null && me.spawnTile !== undefined) {
      conn.reducers.buildCity({ matchId: BigInt(currentMatchId), tile: me.spawnTile });
    }
  }
});

document.getElementById('btnRetreat')!.addEventListener('click', () => {
  if (currentMatchId !== -1 && myPlayerId !== -1) {
    const atk = Array.from(conn.db.attacks.iter()).find(a => Number(a.attacker) === myPlayerId);
    if (atk) {
      conn.reducers.retreatAttack({ matchId: BigInt(currentMatchId), targetPlayer: atk.target });
    }
    statusEl.textContent = 'Retreating all attacks.';
  }
});

document.getElementById('btnChat')!.addEventListener('click', () => {
  chatOpen = !chatOpen;
  chatPanel.classList.toggle('hidden', !chatOpen);
});

document.getElementById('chatSend')!.addEventListener('click', sendChat);
document.getElementById('chatInput')!.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') sendChat();
});

function sendChat() {
  const input = document.getElementById('chatInput') as HTMLInputElement;
  const text = input.value.trim();
  if (text && currentMatchId !== -1) {
    conn.reducers.sendChat({ matchId: BigInt(currentMatchId), text });
    input.value = '';
  }
}

document.getElementById('replayBtn')!.addEventListener('click', () => {
  location.reload();
});

// ── Match list ─────────────────────────────────────────────────────────────────
function renderMatchList() {
  const list = document.getElementById('matchList')!;
  list.innerHTML = '';
  for (const m of conn.db.matches.iter()) {
    if (m.phase !== PHASE_LOBBY) continue;
    const row = document.createElement('div');
    row.className = 'match-row';
    const count = Array.from(conn.db.players.iter()).filter(p => Number(p.matchId) === Number(m.id)).length;
    row.innerHTML = `<span class="mname">${escapeHtml(m.name)}</span><span class="mstatus">${count}/8</span>`;
    row.addEventListener('click', () => {
      currentMatchId = Number(m.id);
      conn.reducers.joinMatch({ matchId: m.id, name: 'Player' });
      enterLobby();
    });
    list.appendChild(row);
  }
  if (list.children.length === 0) {
    list.innerHTML = '<div style="color:#68a;text-align:center;">No open matches.</div>';
  }
}

function enterLobby() {
  hide(startScreen);
  hide(listScreen);
  show(lobbyScreen);
  updateLobbyPlayers();
}

function updateLobbyPlayers() {
  const container = document.getElementById('lobbyPlayers')!;
  container.innerHTML = '';
  for (const p of conn.db.players.iter()) {
    if (Number(p.matchId) !== currentMatchId) continue;
    const el = document.createElement('div');
    el.className = 'lobby-player';
    const color = COLORS[p.color % COLORS.length];
    el.innerHTML = `<div class="dot" style="background:${color}"></div><span class="name">${escapeHtml(p.name)}</span><span class="tag">${p.isBot ? 'BOT' : 'HUMAN'}</span>`;
    container.appendChild(el);
  }
}

// ── Game over ──────────────────────────────────────────────────────────────────
function endGame(won: boolean) {
  gameOver = true;
  hide(hud);
  hide(bottomBar);
  hide(chatPanel);
  show(gameOverScreen);
  document.getElementById('goTitle')!.textContent = won ? '🏆 Victory!' : '💀 Defeat';
  const me = Array.from(conn.db.players.iter()).find(p => Number(p.id) === myPlayerId);
  const tiles = me ? me.tiles : 0;
  const troops = me ? Math.floor(me.troops) : 0;
  const pct = totalLand > 0 ? Math.floor((tiles / totalLand) * 100) : 0;
  document.getElementById('goDetail')!.textContent = won
    ? `You conquered ${pct}% of Earth's land!`
    : `You were eliminated after ${tickN} ticks.`;
  document.getElementById('goStats')!.innerHTML = `
    <div class="stat-item"><div class="label">Tiles</div><div class="val">${tiles}</div></div>
    <div class="stat-item"><div class="label">Troops</div><div class="val">${troops}</div></div>
    <div class="stat-item"><div class="label">Control</div><div class="val">${pct}%</div></div>
    <div class="stat-item"><div class="label">Ticks</div><div class="val">${tickN}</div></div>
  `;
}

// ── Canvas rendering ───────────────────────────────────────────────────────────
function resize() {
  canvas.width = window.innerWidth;
  canvas.height = window.innerHeight;
}
window.addEventListener('resize', resize);
resize();

function worldToScreen(wx: number, wy: number) {
  return {
    x: (wx * view.scale - view.x),
    y: (wy * view.scale - view.y),
  };
}

function screenToWorld(sx: number, sy: number) {
  return {
    x: (sx + view.x) / view.scale,
    y: (sy + view.y) / view.scale,
  };
}

function render() {
  ctx.fillStyle = '#000';
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  if (earthImg) {
    const tl = worldToScreen(0, 0);
    const br = worldToScreen(VISUAL_W, VISUAL_H);
    const w = br.x - tl.x;
    const h = br.y - tl.y;
    ctx.drawImage(earthImg, tl.x, tl.y, w, h);
  }

  if (overlayImageData && overlayCtx) {
    const data = overlayImageData.data;
    data.fill(0);

    for (const chunk of conn.db.tile_chunks.iter()) {
      if (Number(chunk.matchId) !== currentMatchId) continue;
      const cx = chunk.chunkX;
      const cy = chunk.chunkY;
      for (let ly = 0; ly < CHUNK_SIZE; ly++) {
        for (let lx = 0; lx < CHUNK_SIZE; lx++) {
          const tx = cx * CHUNK_SIZE + lx;
          const ty = cy * CHUNK_SIZE + ly;
          if (tx >= SIM_W || ty >= SIM_H) continue;
          const idx = (ly * CHUNK_SIZE + lx);
          const owner = chunk.owners[idx];
          if (owner === 255) continue;
          const color = hex2rgb(COLORS[owner % COLORS.length]);
          const vx = tx * SCALE;
          const vy = ty * SCALE;
          for (let dy = 0; dy < SCALE; dy++) {
            for (let dx = 0; dx < SCALE; dx++) {
              const px = vx + dx;
              const py = vy + dy;
              const pi = (py * VISUAL_W + px) * 4;
              data[pi] = color[0];
              data[pi + 1] = color[1];
              data[pi + 2] = color[2];
              data[pi + 3] = 180;
            }
          }
        }
      }
    }

    overlayCtx.putImageData(overlayImageData, 0, 0);
    const tl = worldToScreen(0, 0);
    const br = worldToScreen(VISUAL_W, VISUAL_H);
    ctx.drawImage(overlayCanvas!, tl.x, tl.y, br.x - tl.x, br.y - tl.y);
  }

  // Minimap
  mmCtx.fillStyle = '#001122';
  mmCtx.fillRect(0, 0, minimap.width, minimap.height);
  const mmScaleX = minimap.width / VISUAL_W;
  const mmScaleY = minimap.height / VISUAL_H;
  for (const chunk of conn.db.tile_chunks.iter()) {
    if (Number(chunk.matchId) !== currentMatchId) continue;
    for (let ly = 0; ly < CHUNK_SIZE; ly++) {
      for (let lx = 0; lx < CHUNK_SIZE; lx++) {
        const idx = (ly * CHUNK_SIZE + lx);
        const owner = chunk.owners[idx];
        if (owner === 255) continue;
        const tx = chunk.chunkX * CHUNK_SIZE + lx;
        const ty = chunk.chunkY * CHUNK_SIZE + ly;
        mmCtx.fillStyle = COLORS[owner % COLORS.length];
        mmCtx.fillRect(tx * mmScaleX, ty * mmScaleY, mmScaleX + 0.5, mmScaleY + 0.5);
      }
    }
  }
  // Viewport rect on minimap
  mmCtx.strokeStyle = '#fff';
  mmCtx.lineWidth = 1;
  const vpx = view.x / view.scale * mmScaleX;
  const vpy = view.y / view.scale * mmScaleY;
  const vpw = canvas.width / view.scale * mmScaleX;
  const vph = canvas.height / view.scale * mmScaleY;
  mmCtx.strokeRect(vpx, vpy, vpw, vph);

  requestAnimationFrame(render);
}

function hex2rgb(hex: string): [number, number, number] {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return [r, g, b];
}

// ── Input ──────────────────────────────────────────────────────────────────────
let dragging = false;
let dragStart = { x: 0, y: 0 };
let viewStart = { x: 0, y: 0 };

canvas.addEventListener('mousedown', (e) => {
  dragging = true;
  dragStart = { x: e.clientX, y: e.clientY };
  viewStart = { x: view.x, y: view.y };
});

window.addEventListener('mousemove', (e) => {
  if (dragging) {
    view.x = viewStart.x - (e.clientX - dragStart.x);
    view.y = viewStart.y - (e.clientY - dragStart.y);
    clampView();
  }
});

window.addEventListener('mouseup', () => { dragging = false; });

canvas.addEventListener('wheel', (e) => {
  e.preventDefault();
  const zoom = e.deltaY > 0 ? 0.9 : 1.1;
  const oldScale = view.scale;
  view.scale *= zoom;
  view.scale = Math.max(0.2, Math.min(5, view.scale));
  const rect = canvas.getBoundingClientRect();
  const mx = e.clientX - rect.left;
  const my = e.clientY - rect.top;
  view.x = mx - (mx + view.x) * (view.scale / oldScale);
  view.y = my - (my + view.y) * (view.scale / oldScale);
  clampView();
}, { passive: false });

function clampView() {
  const maxX = VISUAL_W * view.scale - canvas.width;
  const maxY = VISUAL_H * view.scale - canvas.height;
  view.x = Math.max(0, Math.min(maxX, view.x));
  view.y = Math.max(0, Math.min(maxY, view.y));
}

canvas.addEventListener('click', (e) => {
  if (dragging) return;
  const rect = canvas.getBoundingClientRect();
  const sx = e.clientX - rect.left;
  const sy = e.clientY - rect.top;
  const w = screenToWorld(sx, sy);
  const tx = Math.floor(w.x / SCALE);
  const ty = Math.floor(w.y / SCALE);
  if (tx < 0 || tx >= SIM_W || ty < 0 || ty >= SIM_H) return;
  const tile = ty * SIM_W + tx;

  if (currentMatchId === -1 || myPlayerId === -1) return;

  if (currentPhase === PHASE_SPAWN) {
    conn.reducers.spawn({ matchId: BigInt(currentMatchId), tile });
  } else if (currentPhase === PHASE_PLAYING) {
    // Attack: find who owns this tile
    for (const chunk of conn.db.tile_chunks.iter()) {
      if (Number(chunk.matchId) !== currentMatchId) continue;
      const cx = Math.floor(tx / CHUNK_SIZE);
      const cy = Math.floor(ty / CHUNK_SIZE);
      if (chunk.chunkX !== cx || chunk.chunkY !== cy) continue;
      const lx = tx % CHUNK_SIZE;
      const ly = ty % CHUNK_SIZE;
      const idx = ly * CHUNK_SIZE + lx;
      const owner = chunk.owners[idx];
      if (owner !== 255 && owner !== myPlayerId) {
        conn.reducers.launchAttack({ matchId: BigInt(currentMatchId), targetPlayer: owner });
      }
      break;
    }
  }
});

// Touch support
let touchStartDist = 0;
let touchStartScale = 1;

canvas.addEventListener('touchstart', (e) => {
  if (e.touches.length === 1) {
    dragging = true;
    dragStart = { x: e.touches[0].clientX, y: e.touches[0].clientY };
    viewStart = { x: view.x, y: view.y };
  } else if (e.touches.length === 2) {
    const dx = e.touches[0].clientX - e.touches[1].clientX;
    const dy = e.touches[0].clientY - e.touches[1].clientY;
    touchStartDist = Math.sqrt(dx * dx + dy * dy);
    touchStartScale = view.scale;
  }
}, { passive: false });

canvas.addEventListener('touchmove', (e) => {
  e.preventDefault();
  if (e.touches.length === 1 && dragging) {
    view.x = viewStart.x - (e.touches[0].clientX - dragStart.x);
    view.y = viewStart.y - (e.touches[0].clientY - dragStart.y);
    clampView();
  } else if (e.touches.length === 2) {
    const dx = e.touches[0].clientX - e.touches[1].clientX;
    const dy = e.touches[0].clientY - e.touches[1].clientY;
    const dist = Math.sqrt(dx * dx + dy * dy);
    const zoom = dist / touchStartDist;
    view.scale = Math.max(0.2, Math.min(5, touchStartScale * zoom));
    clampView();
  }
}, { passive: false });

canvas.addEventListener('touchend', () => { dragging = false; });

// ── Boot ───────────────────────────────────────────────────────────────────────
connectStdb();
requestAnimationFrame(render);
