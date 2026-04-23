# 🌍 Blue Marble Front

[![Play Now](https://img.shields.io/badge/▶%20Play%20Now-GitHub%20Pages-00ccff?style=for-the-badge)](https://torantulino.github.io/blue-marble-front/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=for-the-badge)](LICENSE)

> **A browser-based real-time strategy game of global conquest — inspired by OpenFront.io, set on a photorealistic Earth map.**

---

## 🎮 Play

**[👉 https://torantulino.github.io/blue-marble-front/](https://torantulino.github.io/blue-marble-front/)**

No install. No sign-up. Runs in any modern browser on desktop or mobile.

---

## 🗺️ What Is It?

Blue Marble Front is a massively-multiplayer-inspired RTS where you claim territory on a real-world Earth map, grow your population, and expand outward against rival nations. Conquer **80% of all land tiles** to achieve global dominance.

---

## ⚔️ Gameplay

| Action | How |
|--------|-----|
| **Spawn** | Click any unclaimed land tile |
| **Expand** | Your nation auto-expands each tick |
| **Attack** | Click an enemy territory to launch an attack |
| **Retreat** | Press the Retreat button to cancel attacks |
| **Build City** | Spend gold to build a city (boosts troop cap) |
| **Win** | Hold 80% of Earth's land tiles |
| **Lose** | All your tiles get captured |
| **Pan** | Click & drag the map |
| **Zoom** | Scroll wheel / pinch-to-zoom |

---

## 🌐 Features

### M0 Prototype
- 🗺️ **Procedural Earth land mask** — biome-tinted continents with real geography
- 🎨 **Per-nation colour overlay** — see the world get painted in real time
- 🤖 **12 AI nations** — each with their own frontier queue and troop dynamics
- 📊 **Live HUD** — tiles, troops, territory percentage, tick counter
- 🗺️ **Minimap** — viewport-aware overview in the corner
- ⏩ **Variable speed** — up to 8× fast-forward
- 📱 **Mobile-friendly** — touch pan/pinch-zoom fully supported
- 🏆 **Win / defeat screens** with stat summary

### M1 Alpha — SpacetimeDB Multiplayer
- 🔗 **SpacetimeDB backend** — authoritative 10 Hz server-side simulation
- 👥 **Multiplayer matches** — up to 8 players + bots
- 💬 **In-game chat**
- 🏙️ **Cities** — spend gold to increase troop capacity
- 💰 **Passive gold economy** — 100/s humans, 50/s bots
- ⚔️ **Attack & Retreat** — manual combat against specific enemies
- 🤖 **Bot AI** — auto-spawn, expand, and attack

---

## 🛠️ Tech

| | |
|-|---|
| **Renderer** | Canvas 2D — zero dependencies |
| **World model** | 1350×675 tile equirectangular grid (chunked 32×32) |
| **Backend** | SpacetimeDB Rust module (10 Hz tick) |
| **Client** | Vite + TypeScript |
| **Combat** | Troop-strength comparison with frontline attrition |
| **Troop model** | `max = 2×(tiles^0.6×1000+50000) + cities×250k`, regen tapers to capacity |
| **Deployment** | GitHub Pages (client) + SpacetimeDB Cloud (backend) |

---

## 🏗️ Development

### Prerequisites
- [Rust](https://rustup.rs/) + [SpacetimeDB CLI](https://spacetimedb.com/docs/getting-started)
- [Node.js](https://nodejs.org/) 18+

### 1. Run the SpacetimeDB module locally

```bash
cd spacetime-module
spacetime build
spacetime publish --project-path . blue-marble-front
```

> The Rust module embeds a pre-baked NASA ocean mask at `spacetime-module/assets/ocean_mask_1350x675.bin` (~114 KB). It's committed to the repo so normal builds work offline. If you ever need to regenerate it (source URL changes, or SIM resolution changes):
>
> ```bash
> node scripts/bake-ocean-mask.mjs
> ```
>
> That fetches the NASA landmask PNG, downsamples to 1350×675, and writes the bitfield. Commit the updated `.bin`.

### 2. Run the client

```bash
cd client
npm install
npm run dev
```

The client will connect to `ws://localhost:3000` by default. Adjust the WebSocket URL in `src/main.ts` if your SpacetimeDB host differs.

### 3. Generate TypeScript bindings (recommended)

Instead of the lightweight wrapper in `src/main.ts`, you can generate official SDK bindings:

```bash
spacetime generate --lang typescript --out-dir ../client/src/generated --project-path ./spacetime-module
```

Then replace the custom `SpacetimeDBClient` with the generated SDK.

---

## 🗺️ Roadmap

- [x] **M0** — Prototype: local sim, Canvas 2D, AI nations
- [x] **M1** — Alpha: SpacetimeDB multiplayer, 8 players, growth + attack + retreat, passive gold, cities
- [ ] **M2** — Beta: Ports, Warships, Trade Ships, Defence Posts, alliances, embargos, donate, chat
- [ ] **M3** — Pre-launch: Silos, SAMs, Atom/Hydrogen/MIRV, Factories + Trains, replays, cosmetics shop
- [ ] **M4** — Launch: 100-player public match, marketing, localisation 8 langs
- [ ] **M5** — Season 1: Persistent globe meta, battle pass, mobile polish

---

## 📄 License

MIT © 2026 Torantulino
---

