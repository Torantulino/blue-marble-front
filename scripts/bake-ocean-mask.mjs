// Bake two assets the Rust module pulls in via include_bytes!():
//
//   spacetime-module/assets/ocean_mask_1350x675.bin  (1 bit per tile)
//       1 = land, 0 = ocean, row-major
//
//   spacetime-module/assets/terrain_1350x675.bin     (1 byte per tile)
//       0 = ocean
//       1 = plains   (green / farmland — easy mag & speed)
//       2 = highland (tan / brown / dark — medium)
//       3 = mountain (white / ice caps / high peaks — hard)
//
// The ocean mask comes from NEO's oceanmask PNG. The terrain classification
// reads the colour of the corresponding block in NASA's Blue Marble May 2004
// 5400×2700 JPG and maps RGB → a difficulty tier. The mapping is intended to
// be "roughly what you see": snow/ice is hardest, bare/rocky terrain is
// medium, vegetated land is easiest.
//
// Run once to produce both committed .bins. Re-run only if the source URLs
// or SIM resolution changes.
//
//   npm run bake:mask

import { PNG } from 'pngjs';
import jpeg from 'jpeg-js';
import { writeFile } from 'node:fs/promises';
import { Readable } from 'node:stream';

const MASK_URL = 'https://neo.gsfc.nasa.gov/archive/bluemarble/bmng/landmask/world.oceanmask.5400x2700.png';
const VISUAL_URL = 'https://assets.science.nasa.gov/content/dam/science/esd/eo/images/bmng/bmng-base/may/world.200405.3x5400x2700.jpg';
const SRC_W = 5400, SRC_H = 2700;
const SIM_W = 1350, SIM_H = 675;
const BLOCK = SRC_W / SIM_W; // 4
const OCEAN_OUT = 'spacetime-module/assets/ocean_mask_1350x675.bin';
const TERRAIN_OUT = 'spacetime-module/assets/terrain_1350x675.bin';

// ── Fetch + decode ocean mask (PNG) ─────────────────────────────────────────
console.log('Fetching NASA ocean mask from', MASK_URL);
const maskRes = await fetch(MASK_URL);
if (!maskRes.ok) throw new Error(`Failed to fetch mask: ${maskRes.status}`);
const maskBuf = Buffer.from(await maskRes.arrayBuffer());
console.log('  downloaded', maskBuf.byteLength, 'bytes');

const maskPng = await new Promise((resolve, reject) => {
  const p = new PNG();
  p.on('parsed', function () { resolve(this); });
  p.on('error', reject);
  Readable.from(maskBuf).pipe(p);
});
if (maskPng.width !== SRC_W || maskPng.height !== SRC_H) {
  throw new Error(`Unexpected mask dims ${maskPng.width}×${maskPng.height}, want ${SRC_W}×${SRC_H}`);
}
console.log('  decoded PNG', maskPng.width, '×', maskPng.height);

// Despite the file being called "oceanmask", the payload is: bright = land,
// dark = ocean. M0's prototype used the same reading.
const isLandPixel = (sx, sy) => {
  const i = (sy * SRC_W + sx) * 4;
  const bright = (maskPng.data[i] + maskPng.data[i + 1] + maskPng.data[i + 2]) / 3;
  return bright >= 128;
};

// ── Fetch + decode Blue Marble visual (JPG) ─────────────────────────────────
console.log('Fetching NASA Blue Marble visual from', VISUAL_URL);
const visRes = await fetch(VISUAL_URL);
if (!visRes.ok) throw new Error(`Failed to fetch visual: ${visRes.status}`);
const visBuf = Buffer.from(await visRes.arrayBuffer());
console.log('  downloaded', visBuf.byteLength, 'bytes');

const vis = jpeg.decode(visBuf, { useTArray: true });
if (vis.width !== SRC_W || vis.height !== SRC_H) {
  throw new Error(`Unexpected visual dims ${vis.width}×${vis.height}, want ${SRC_W}×${SRC_H}`);
}
console.log('  decoded JPG', vis.width, '×', vis.height);

// ── Bake ────────────────────────────────────────────────────────────────────
// Classify a (4×4 averaged) RGB triplet into { plains, highland, mountain }.
// Tuned against NASA May-2004 Blue Marble:
//   - Snow/ice caps, glaciers, very high-albedo clouds-free peaks: near-white.
//   - Vegetated land (boreal forest / savanna / rainforest): green-dominant.
//   - Desert / arid / exposed rock / high-lat tundra: tan-to-brown or dark grey.
function classifyTerrain(r, g, b) {
  const lum = (r + g + b) / 3;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const sat = max === 0 ? 0 : (max - min) / max;

  // Near-white / ice / glaciers: very bright AND very desaturated.
  // Antarctica, Greenland, Himalayan / Andean snowpack. Sahara highlights
  // are bright but warm-toned (saturation ~0.2) so they don't qualify.
  if (min > 215 && sat < 0.08) return 3;
  if (lum > 225 && sat < 0.05) return 3;

  // Tan / brown / ochre: desert & exposed rock (R > G > B with a margin).
  // Sahara, Arabia, central Australia, Atacama, Gobi, alpine rocky terrain.
  if (r > g && g >= b && r - b > 25) return 2;

  // Green-dominant: vegetation (plains — forest, farmland, savanna, tundra).
  // NASA imagery is desaturated so we accept "G competitive or dominant".
  if (g >= r - 3 && g >= b) return 1;

  // Default to plains. Mixed vegetation / muted green-browns default here
  // because the game is more fun when most of the world is traversable.
  return 1;
}

const landBitfield = new Uint8Array(Math.ceil(SIM_W * SIM_H / 8));
const terrain = new Uint8Array(SIM_W * SIM_H);
const counts = [0, 0, 0, 0];

for (let ty = 0; ty < SIM_H; ty++) {
  for (let tx = 0; tx < SIM_W; tx++) {
    let landVotes = 0;
    let rSum = 0, gSum = 0, bSum = 0;
    for (let dy = 0; dy < BLOCK; dy++) {
      for (let dx = 0; dx < BLOCK; dx++) {
        const sx = tx * BLOCK + dx;
        const sy = ty * BLOCK + dy;
        if (isLandPixel(sx, sy)) landVotes++;
        const vi = (sy * SRC_W + sx) * 4;
        rSum += vis.data[vi];
        gSum += vis.data[vi + 1];
        bSum += vis.data[vi + 2];
      }
    }
    const bitIdx = ty * SIM_W + tx;
    const pixelCount = BLOCK * BLOCK;

    if (landVotes >= pixelCount / 2) {
      landBitfield[bitIdx >> 3] |= 1 << (bitIdx & 7);
      const r = rSum / pixelCount;
      const g = gSum / pixelCount;
      const b = bSum / pixelCount;
      const t = classifyTerrain(r, g, b);
      terrain[bitIdx] = t;
      counts[t]++;
    } else {
      // Ocean: leave terrain[bitIdx] = 0 and land bit = 0.
      counts[0]++;
    }
  }
}

await writeFile(OCEAN_OUT, landBitfield);
await writeFile(TERRAIN_OUT, terrain);

const total = SIM_W * SIM_H;
const pct = (n) => (n / total * 100).toFixed(1);
console.log('\nWrote', OCEAN_OUT, `(${landBitfield.byteLength} bytes)`);
console.log('Wrote', TERRAIN_OUT, `(${terrain.byteLength} bytes)`);
console.log('Terrain distribution:');
console.log(`  0 ocean    : ${counts[0].toString().padStart(7)} (${pct(counts[0])}%)`);
console.log(`  1 plains   : ${counts[1].toString().padStart(7)} (${pct(counts[1])}%)`);
console.log(`  2 highland : ${counts[2].toString().padStart(7)} (${pct(counts[2])}%)`);
console.log(`  3 mountain : ${counts[3].toString().padStart(7)} (${pct(counts[3])}%)`);
