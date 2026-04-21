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

Blue Marble Front is a massively-multiplayer-inspired RTS where you claim territory on a real-world Earth map, grow your population, and expand outward against AI rival nations. Conquer **80% of all land tiles** to achieve global dominance.

---

## ⚔️ Gameplay

| Action | How |
|--------|-----|
| **Spawn** | Click any unclaimed land tile (green) |
| **Expand** | Your nation auto-expands each tick |
| **Win** | Hold 80% of Earth's land tiles |
| **Lose** | All your tiles get captured |
| **Pan** | Click & drag the map |
| **Zoom** | Scroll wheel / pinch-to-zoom |
| **Speed** | ½× / 2× buttons at the bottom |

Choose **Easy**, **Normal**, or **Hard** before starting — difficulty scales AI aggression and expansion rate.

---

## 🌐 Features (M0 Prototype)

- 🗺️ **Procedural Earth land mask** — biome-tinted continents with real geography
- 🎨 **Per-nation colour overlay** — see the world get painted in real time
- 🤖 **12 AI nations** — each with their own frontier queue and troop dynamics
- 📊 **Live HUD** — tiles, troops, territory percentage, tick counter
- 🗺️ **Minimap** — viewport-aware overview in the corner
- ⏩ **Variable speed** — up to 8× fast-forward
- 📱 **Mobile-friendly** — touch pan/pinch-zoom fully supported
- 🏆 **Win / defeat screens** with stat summary

---

## 🛠️ Tech

| | |
|-|---|
| **Renderer** | Canvas 2D — zero dependencies |
| **World model** | 540×270 tile equirectangular grid |
| **Combat** | Troop-strength comparison with frontline attrition |
| **Troop model** | `max = 2×(tiles^0.6×1000+50000)`, regen tapers to capacity |
| **Land mask** | Seeded procedural generation from continent bounding boxes |
| **Deployment** | GitHub Pages (single `index.html`) |

---

## 🗺️ Roadmap

- [ ] **M1** — SpacetimeDB multiplayer backend (Rust, 10 Hz tick)
- [ ] **M2** — NASA Blue Marble WebGL globe (three.js icosphere)
- [ ] **M3** — Structures: cities, ports, missile silos
- [ ] **M4** — Diplomacy, alliances, nukes
- [ ] **M5** — Full multiplayer matchmaking & live ops

---

## 📄 License

MIT © 2026 Torantulino
