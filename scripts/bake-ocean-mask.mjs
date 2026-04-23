// Bake the NASA ocean mask into a bitfield the Rust module can include_bytes!().
// Output: spacetime-module/assets/ocean_mask_1350x675.bin
//   - 1 bit per sim tile, row-major (y-major, then x)
//   - 1 = land, 0 = ocean (or out-of-bounds)
//   - Resolution 1350×675 matches SIM_W/SIM_H in lib.rs
//   - Downsample: each sim tile aggregates 4×4 source pixels; tile is land iff
//     >= 8 of those 16 source pixels are land in the NASA mask.
//
// Run once to produce the committed .bin. Re-run only if the source URL or
// SIM resolution changes.
//
//   node scripts/bake-ocean-mask.mjs
//
// The NASA ocean mask encodes white = ocean, black = land (see archive/m0.html
// for the M0 prototype's original use of this asset).

import { PNG } from 'pngjs';
import { writeFile } from 'node:fs/promises';
import { Readable } from 'node:stream';

const MASK_URL = 'https://neo.gsfc.nasa.gov/archive/bluemarble/bmng/landmask/world.oceanmask.5400x2700.png';
const SRC_W = 5400, SRC_H = 2700;
const SIM_W = 1350, SIM_H = 675;
const BLOCK = SRC_W / SIM_W; // 4
const OUT = 'spacetime-module/assets/ocean_mask_1350x675.bin';

console.log('Fetching NASA ocean mask from', MASK_URL);
const res = await fetch(MASK_URL);
if (!res.ok) throw new Error(`Failed to fetch mask: ${res.status}`);
const buf = Buffer.from(await res.arrayBuffer());
console.log('Downloaded', buf.byteLength, 'bytes');

const png = await new Promise((resolve, reject) => {
  const p = new PNG();
  p.on('parsed', function () { resolve(this); });
  p.on('error', reject);
  Readable.from(buf).pipe(p);
});

if (png.width !== SRC_W || png.height !== SRC_H) {
  throw new Error(`Unexpected mask dims ${png.width}×${png.height}, want ${SRC_W}×${SRC_H}`);
}
console.log('Decoded PNG', png.width, '×', png.height);

// Despite the file being called "oceanmask", the payload is: bright pixel = land,
// dark pixel = ocean. M0's landMask marks bright pixels as 1 and treats them as
// spawnable (= land), so we match that semantic.
const isLand = (sx, sy) => {
  const i = (sy * SRC_W + sx) * 4;
  const bright = (png.data[i] + png.data[i + 1] + png.data[i + 2]) / 3;
  return bright >= 128;
};

const totalBits = SIM_W * SIM_H;
const out = new Uint8Array(Math.ceil(totalBits / 8));
let landCount = 0;
for (let ty = 0; ty < SIM_H; ty++) {
  for (let tx = 0; tx < SIM_W; tx++) {
    let landVotes = 0;
    for (let dy = 0; dy < BLOCK; dy++) {
      for (let dx = 0; dx < BLOCK; dx++) {
        if (isLand(tx * BLOCK + dx, ty * BLOCK + dy)) landVotes++;
      }
    }
    if (landVotes >= (BLOCK * BLOCK) / 2) {
      const bitIdx = ty * SIM_W + tx;
      out[bitIdx >> 3] |= 1 << (bitIdx & 7);
      landCount++;
    }
  }
}

await writeFile(OUT, out);
console.log(`Wrote ${OUT} (${out.byteLength} bytes, ${landCount} land tiles out of ${totalBits}, ${(landCount / totalBits * 100).toFixed(1)}%)`);
