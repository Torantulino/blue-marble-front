use spacetimedb::{ReducerContext, Table, Timestamp, Identity, TimeDuration, ScheduleAt};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

// ── Constants ────────────────────────────────────────────────────────────────
const SIM_W: u32 = 1350;
const SIM_H: u32 = 675;
const CHUNK_SIZE: u32 = 32;
const CHUNKS_X: u32 = (SIM_W + CHUNK_SIZE - 1) / CHUNK_SIZE; // 43
const CHUNKS_Y: u32 = (SIM_H + CHUNK_SIZE - 1) / CHUNK_SIZE; // 22
const MAX_PLAYERS: u32 = 13; // 12 AI + 1 human, matching M0 prototype.
const WIN_PCT: f32 = 0.80;
const MIN_SPAWN_DIST: u32 = 30;
const TICK_MS: u64 = 100;

// Troop / economy constants
const START_TROOPS_HUMAN: f32 = 25_000.0;
const START_TROOPS_BOT: f32 = 10_000.0;
const GOLD_PER_TICK_HUMAN: f32 = 10.0; // 100/s at 10 Hz
const GOLD_PER_TICK_BOT: f32 = 5.0;    // 50/s at 10 Hz
const CITY_COST_BASE: f32 = 125_000.0;
const CITY_COST_MAX: f32 = 1_000_000.0;
const CITY_BUILD_TICKS: u32 = 20;
const CITY_TROOP_BONUS: f32 = 250_000.0;

// Phase constants
const PHASE_LOBBY: u8 = 0;
const PHASE_SPAWN: u8 = 1;
const PHASE_PLAYING: u8 = 2;
const PHASE_ENDED: u8 = 3;

// Difficulty (stored on Match.difficulty): 0=Easy, 1=Normal, 2=Hard.
// Applied to human spawn troops; bot expansion pacing lives in TribeExecution.
const DIFF_HUMAN_TROOPS_MULT: [f32; 3] = [1.4, 1.0, 0.7];

// Speed multiplier (stored on Match.speed_multiplier, quarter-scale):
// 1=0.25×, 2=0.5×, 4=1×, 8=2×, 16=4×. See sub_tick_count().
const DEFAULT_SPEED: u8 = 4;

// Cap on how many chunks a single player tracks in `owned_chunks`. The whole
// world is 946 chunks, so ~1024 is a hard upper bound.
const OWNED_CHUNKS_CAP: usize = 1024;

// ── OpenFront-style attack mechanics ────────────────────────────────────────

// Terrain types (stored per tile in TileChunk.terrain).
// 1/2/3 are used via TERRAIN_MAG / SPEED / ENQUEUE_MAG arrays below.
const TERRAIN_OCEAN: u8 = 0;
const _TERRAIN_PLAINS: u8 = 1;
const _TERRAIN_HIGHLAND: u8 = 2;
const _TERRAIN_MOUNTAIN: u8 = 3;

// Per-terrain combat weights, ported from OF DefaultConfig.ts attackLogic.
// Plains: easy/fast. Mountain: hard/slow. Highland: between.
const TERRAIN_MAG: [f32; 4] = [0.0, 80.0, 100.0, 120.0];
const TERRAIN_SPEED: [f32; 4] = [0.0, 16.5, 20.0, 25.0];
const TERRAIN_ENQUEUE_MAG: [f32; 4] = [0.0, 1.0, 1.5, 2.0];

// Retreat refund: player-target attacks lose 25% on retreat; wilderness
// is refunded in full.
const RETREAT_MALUS: f32 = 0.25;

// attack_amount(player) = player.troops / divisor.
const ATTACK_AMOUNT_HUMAN_DIVISOR: f32 = 5.0;
const ATTACK_AMOUNT_BOT_DIVISOR: f32 = 20.0;

// When a defender drops below this many tiles, mark them eliminated.
// Full cluster cleanup + tile handoff deferred to a follow-up PR.
const DEFENDER_DEAD_TILES_THRESH: u32 = 100;

// Large-nation combat modifier pivot. Matches OF's LARGE_TILE_BREAKPOINT.
const LARGE_TILE_BREAKPOINT: f32 = 100_000.0;

// Defense sigmoid inputs (OF DefaultConfig tunables). A larger midpoint lets
// smaller nations run longer before the "large defender" debuff kicks in.
const DEFENSE_DEBUFF_DECAY: f32 = 50_000.0;
const DEFENSE_DEBUFF_MIDPOINT: f32 = 20_000.0;

// Human-attacks-bot mag modifier (softer combat vs bots).
const HUMAN_VS_BOT_MAG_MULT: f32 = 0.8;

// Bot AI pacing — a bot issues at most one attack per cooldown window.
const BOT_ATTACK_COOLDOWN_MIN_TICKS: u64 = 40;
const BOT_ATTACK_COOLDOWN_MAX_TICKS: u64 = 80;

// Cap on Attack.border and Attack.to_conquer_* to bound persisted row size.
const ATTACK_HEAP_CAP: usize = 4096;

// Target-id sentinel for wilderness (TerraNullius). Matches OF's TN id = 0.
const TARGET_WILDERNESS: u32 = 0;

// Terrain byte array. 1350×675 bytes = 911_250 bytes. One byte per tile:
// 0=ocean, 1=plains, 2=highland, 3=mountain. Baked from the NASA Blue Marble
// visual by scripts/bake-ocean-mask.mjs (which also consults the ocean mask
// to decide land vs ocean per tile).
const TERRAIN: &[u8] = include_bytes!("../assets/terrain_1350x675.bin");

fn mask_terrain(tx: u32, ty: u32) -> u8 {
    if tx >= SIM_W || ty >= SIM_H { return TERRAIN_OCEAN; }
    TERRAIN[(ty * SIM_W + tx) as usize]
}

// ── Helpers ──────────────────────────────────────────────────────────────────
fn tile_idx(x: u32, y: u32) -> u32 { y * SIM_W + x }
fn chunk_idx(cx: u32, cy: u32) -> u32 { cy * CHUNKS_X + cx }
fn tile_to_chunk(tx: u32, ty: u32) -> (u32, u32, u32, u32) {
    let cx = tx / CHUNK_SIZE;
    let cy = ty / CHUNK_SIZE;
    let lx = tx % CHUNK_SIZE;
    let ly = ty % CHUNK_SIZE;
    (cx, cy, lx, ly)
}

// Max troops a player can regenerate up to, ported from OF DefaultConfig.
// Bots get 1/3 of the human curve so they don't snowball unbounded.
fn compute_max_troops(tiles: u32, city_levels: u32, is_bot: bool) -> f32 {
    let t = tiles as f32;
    let base = 2.0 * (t.powf(0.7) * 1000.0 + 50_000.0)
             + (city_levels as f32) * CITY_TROOP_BONUS;
    if is_bot { base / 3.0 } else { base }
}

// Per-tick regen. Bots regen 60% as fast as humans.
fn regen_rate(troops: f32, max_troops: f32, is_bot: bool) -> f32 {
    let base = 10.0 + troops.powf(0.8) / 4.0;
    let taper = 1.0 - (troops / max_troops.max(1.0)).min(1.0);
    let scale = if is_bot { 0.6 } else { 1.0 };
    base * taper.max(0.0) * scale
}

// OpenFront's attack_amount: troops committed when an attack is launched.
fn attack_amount(player: &Player) -> f32 {
    let divisor = if player.is_bot {
        ATTACK_AMOUNT_BOT_DIVISOR
    } else {
        ATTACK_AMOUNT_HUMAN_DIVISOR
    };
    (player.troops / divisor).max(0.0)
}

// Are the attack's current target and the tile's current owner compatible —
// i.e. can this attack capture that tile? Wilderness attacks claim unclaimed
// land; player-target attacks only capture that player's tiles.
fn is_capturable(target_id: u32, current_owner: Option<u32>) -> bool {
    match (target_id, current_owner) {
        (TARGET_WILDERNESS, None) => true,
        (t, Some(o)) if t != TARGET_WILDERNESS && o == t => true,
        _ => false,
    }
}

fn has_attacker_neighbor(cache: &mut ChunkCache, attacker_id: u32, tx: u32, ty: u32) -> bool {
    let dirs = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)];
    for (dx, dy) in dirs {
        let nx = tx as i32 + dx;
        let ny = ty as i32 + dy;
        if nx < 0 || nx >= SIM_W as i32 || ny < 0 || ny >= SIM_H as i32 { continue; }
        if cache.get_owner(nx as u32, ny as u32) == Some(attacker_id) { return true; }
    }
    false
}

fn sigmoid(x: f32, decay: f32, midpoint: f32) -> f32 {
    let z = (x - midpoint) / decay;
    1.0 / (1.0 + (-z).exp())
}

// Outcome of processing a single tile during an attack tick.
struct AttackStep {
    attacker_loss: f32,
    defender_loss: f32,
    tiles_used: f32,
}

// Port of OF DefaultConfig.ts attackLogic. Wilderness target takes the simpler
// branch (no defender_loss, no large-player debuffs). MVP omits defense-post,
// traitor, and fallout modifiers (they default to 1.0 until those systems
// exist).
fn attack_logic(
    atk_troops: f32,
    attacker: &Player,
    target: Option<&Player>,
    terrain: u8,
) -> AttackStep {
    let t_idx = (terrain.min(3)) as usize;
    let mut mag = TERRAIN_MAG[t_idx];
    let speed = TERRAIN_SPEED[t_idx];
    let atk = atk_troops.max(1.0);

    if let Some(t) = target {
        if !attacker.is_bot && t.is_bot {
            mag *= HUMAN_VS_BOT_MAG_MULT;
        }
        let defense_sig = 1.0 - sigmoid(t.tiles as f32, DEFENSE_DEBUFF_DECAY, DEFENSE_DEBUFF_MIDPOINT);
        let large_defender_attack_debuff = 0.7 + 0.3 * defense_sig;
        let large_defender_speed_debuff  = 0.7 + 0.3 * defense_sig;
        let large_attack_bonus = if attacker.tiles as f32 > LARGE_TILE_BREAKPOINT {
            (LARGE_TILE_BREAKPOINT / attacker.tiles as f32).powf(0.35)
        } else { 1.0 };
        let large_attacker_speed = if attacker.tiles as f32 > LARGE_TILE_BREAKPOINT {
            (LARGE_TILE_BREAKPOINT / attacker.tiles as f32).powf(0.6)
        } else { 1.0 };
        let traitor_mod = 1.0; // Deferred: traitor system.
        let def_troops = t.troops.max(1.0);
        let defender_troop_loss = def_troops / (t.tiles.max(1) as f32);
        let current_attacker_loss = (def_troops / atk).clamp(0.6, 2.0)
            * mag * 0.8
            * large_defender_attack_debuff
            * large_attack_bonus
            * traitor_mod;
        let alt_attacker_loss = 1.3 * defender_troop_loss * (mag / 100.0) * traitor_mod;
        let attacker_loss = 0.4 * current_attacker_loss + 0.6 * alt_attacker_loss;
        let tiles_used = (def_troops / (5.0 * atk)).clamp(0.2, 1.5)
            * speed
            * large_defender_speed_debuff
            * large_attacker_speed
            * traitor_mod;
        AttackStep { attacker_loss, defender_loss: defender_troop_loss, tiles_used }
    } else {
        // Wilderness: cheap per-tile, no defender to bleed.
        let attacker_loss = if attacker.is_bot { mag / 10.0 } else { mag / 5.0 };
        let tiles_used = (2000.0 * speed.max(10.0) / atk).clamp(5.0, 100.0);
        AttackStep { attacker_loss, defender_loss: 0.0, tiles_used }
    }
}

// Port of OF attackTilesPerTick (DefaultConfig.ts:747).
fn attack_tiles_per_tick(atk_troops: f32, target: Option<&Player>, num_adjacent: u32) -> f32 {
    if let Some(t) = target {
        let def = t.troops.max(1.0);
        let ratio = (10.0 * atk_troops / def).clamp(0.01, 0.5);
        ratio * (num_adjacent as f32) * 3.0
    } else {
        (num_adjacent as f32) * 2.0
    }
}

// Priority for a tile-to-conquer. Lower values pop first (min-heap in step_match).
// Ports OF's formula in AttackExecution.addNeighbors: tiles with more attacker
// 4-neighbours already around them are lower priority (pop sooner), mountain
// terrain is higher priority (pop later = slower advance through mountains).
fn compute_tile_priority(cache: &mut ChunkCache, attacker_id: u32, tx: u32, ty: u32, current_tick: u64) -> i32 {
    let terrain = cache.terrain_at(tx, ty);
    let enqueue_mag = TERRAIN_ENQUEUE_MAG[(terrain.min(3)) as usize];
    let dirs = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)];
    let mut count = 0;
    for (dx, dy) in dirs {
        let nx = tx as i32 + dx;
        let ny = ty as i32 + dy;
        if nx < 0 || nx >= SIM_W as i32 || ny < 0 || ny >= SIM_H as i32 { continue; }
        if cache.get_owner(nx as u32, ny as u32) == Some(attacker_id) { count += 1; }
    }
    // Deterministic jitter so refreshes yield stable priorities per tile.
    let seed = rng_seed(tx as u64, ty as u64, current_tick);
    let base = ((seed % 7) as f32) + 10.0;
    let priority = base * (1.0 - 0.5 * count as f32 + enqueue_mag / 2.0) + current_tick as f32;
    priority as i32
}

// After a tile is claimed during an attack, enqueue its target-capturable
// 4-neighbours (land + owner matches target). Returns (tile, priority) pairs.
fn enqueue_tile_neighbors(
    cache: &mut ChunkCache,
    attacker_id: u32,
    target_id: u32,
    tx: u32,
    ty: u32,
    current_tick: u64,
) -> Vec<(u32, i32)> {
    let mut out = Vec::new();
    let dirs = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)];
    for (dx, dy) in dirs {
        let nx = tx as i32 + dx;
        let ny = ty as i32 + dy;
        if nx < 0 || nx >= SIM_W as i32 || ny < 0 || ny >= SIM_H as i32 { continue; }
        let nxu = nx as u32;
        let nyu = ny as u32;
        if !cache.is_land(nxu, nyu) { continue; }
        let owner = cache.get_owner(nxu, nyu);
        if !is_capturable(target_id, owner) { continue; }
        let prio = compute_tile_priority(cache, attacker_id, nxu, nyu, current_tick);
        out.push((nyu * SIM_W + nxu, prio));
    }
    out
}

// Scan the attacker's owned_chunks for capturable 4-neighbours. Returns the
// deduped border set and the initial heap entries.
fn seed_attack_border(
    cache: &mut ChunkCache,
    attacker: &Player,
    target_id: u32,
    _source_tile: Option<u32>, // MVP: always None
    current_tick: u64,
) -> (Vec<u32>, Vec<(u32, i32)>) {
    let mut border_set: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut heap: Vec<(u32, i32)> = Vec::new();
    let attacker_byte = attacker.id as u8;
    let chunk_ids: Vec<u64> = attacker.owned_chunks.clone();
    let dirs = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)];

    'outer: for chunk_id in chunk_ids {
        let owners = match cache.clone_owners(chunk_id) {
            Some(o) => o,
            None => continue,
        };
        let local_idx = (chunk_id & 0xFFFF_FFFF) as u32;
        let cx = local_idx % CHUNKS_X;
        let cy = local_idx / CHUNKS_X;
        for ly in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let idx = (ly * CHUNK_SIZE + lx) as usize;
                if owners.get(idx).copied().unwrap_or(255) != attacker_byte { continue; }
                let tx = cx * CHUNK_SIZE + lx;
                let ty = cy * CHUNK_SIZE + ly;
                if tx >= SIM_W || ty >= SIM_H { continue; }
                for (dx, dy) in dirs {
                    let nx = tx as i32 + dx;
                    let ny = ty as i32 + dy;
                    if nx < 0 || nx >= SIM_W as i32 || ny < 0 || ny >= SIM_H as i32 { continue; }
                    let nxu = nx as u32;
                    let nyu = ny as u32;
                    if !cache.is_land(nxu, nyu) { continue; }
                    let owner = cache.get_owner(nxu, nyu);
                    if !is_capturable(target_id, owner) { continue; }
                    let key = nyu * SIM_W + nxu;
                    if border_set.insert(key) {
                        let prio = compute_tile_priority(cache, attacker.id, nxu, nyu, current_tick);
                        heap.push((key, prio));
                        if heap.len() >= ATTACK_HEAP_CAP { break 'outer; }
                    }
                }
            }
        }
    }

    let border: Vec<u32> = border_set.into_iter().collect();
    (border, heap)
}

fn city_cost(nth: u32) -> f32 {
    let cost = CITY_COST_BASE * (2_u32.pow(nth) as f32);
    cost.min(CITY_COST_MAX)
}

fn dist_sq(a: (u32, u32), b: (u32, u32)) -> u32 {
    let dx = if a.0 > b.0 { a.0 - b.0 } else { b.0 - a.0 };
    let dy = if a.1 > b.1 { a.1 - b.1 } else { b.1 - a.1 };
    dx * dx + dy * dy
}

fn rng_seed(match_id: u64, tick: u64, salt: u64) -> u64 {
    match_id.wrapping_mul(6364136223846793005).wrapping_add(tick.wrapping_mul(1442695040888963407)).wrapping_add(salt)
}

// ── ChunkCache ──────────────────────────────────────────────────────────────
// Read-through / write-back cache for TileChunk rows, scoped to a single
// reducer body (usually one `step_match` call). Collapses redundant chunk
// reads and, crucially, batches tile ownership edits: N set_owner calls into
// the same chunk produce exactly one DB write at flush time, instead of N
// full 1024-byte Vec rewrites.

struct ChunkCacheEntry {
    chunk: TileChunk,
    dirty: bool,
}

struct ChunkCache<'a> {
    ctx: &'a ReducerContext,
    match_id: u64,
    entries: HashMap<u64, ChunkCacheEntry>,
}

impl<'a> ChunkCache<'a> {
    fn new(ctx: &'a ReducerContext, match_id: u64) -> Self {
        Self { ctx, match_id, entries: HashMap::new() }
    }

    fn full_chunk_id(&self, tx: u32, ty: u32) -> Option<u64> {
        if tx >= SIM_W || ty >= SIM_H { return None; }
        let cx = tx / CHUNK_SIZE;
        let cy = ty / CHUNK_SIZE;
        Some((self.match_id << 32) | (chunk_idx(cx, cy) as u64))
    }

    // Lazily load a chunk into the cache. Returns None only if the DB has no
    // such row (should never happen after generate_terrain).
    fn load(&mut self, chunk_id: u64) -> Option<&mut ChunkCacheEntry> {
        if !self.entries.contains_key(&chunk_id) {
            let chunk = self.ctx.db.tile_chunks().id().find(chunk_id)?;
            self.entries.insert(chunk_id, ChunkCacheEntry { chunk, dirty: false });
        }
        self.entries.get_mut(&chunk_id)
    }

    fn is_land(&mut self, tx: u32, ty: u32) -> bool {
        self.terrain_at(tx, ty) != TERRAIN_OCEAN
    }

    fn terrain_at(&mut self, tx: u32, ty: u32) -> u8 {
        let Some(chunk_id) = self.full_chunk_id(tx, ty) else { return TERRAIN_OCEAN; };
        let Some(entry) = self.load(chunk_id) else { return TERRAIN_OCEAN; };
        let lx = tx % CHUNK_SIZE;
        let ly = ty % CHUNK_SIZE;
        let idx = (ly * CHUNK_SIZE + lx) as usize;
        entry.chunk.terrain.get(idx).copied().unwrap_or(TERRAIN_OCEAN)
    }

    fn get_owner(&mut self, tx: u32, ty: u32) -> Option<u32> {
        let chunk_id = self.full_chunk_id(tx, ty)?;
        let entry = self.load(chunk_id)?;
        let lx = tx % CHUNK_SIZE;
        let ly = ty % CHUNK_SIZE;
        let idx = (ly * CHUNK_SIZE + lx) as usize;
        match entry.chunk.owners.get(idx).copied() {
            Some(255) | None => None,
            Some(o) => Some(o as u32),
        }
    }

    /// Set the tile to `new_owner`. Returns the (chunk_id, previous_owner)
    /// so callers can maintain `Player.owned_chunks` sets correctly.
    /// previous_owner = None when the tile was unclaimed.
    fn set_owner(&mut self, tx: u32, ty: u32, new_owner: u32) -> Option<(u64, Option<u32>)> {
        let chunk_id = self.full_chunk_id(tx, ty)?;
        let entry = self.load(chunk_id)?;
        let lx = tx % CHUNK_SIZE;
        let ly = ty % CHUNK_SIZE;
        let idx = (ly * CHUNK_SIZE + lx) as usize;
        if idx >= entry.chunk.owners.len() { return None; }
        let prev = entry.chunk.owners[idx];
        let prev_owner = if prev == 255 { None } else { Some(prev as u32) };
        if prev as u32 == new_owner { return Some((chunk_id, prev_owner)); }
        entry.chunk.owners[idx] = new_owner as u8;
        entry.dirty = true;
        Some((chunk_id, prev_owner))
    }

    /// Copy the 1024-byte owners vec for bulk iteration. Avoids borrow
    /// conflicts when the caller wants to probe neighbour ownership in the
    /// same chunk via `get_owner`.
    fn clone_owners(&mut self, chunk_id: u64) -> Option<Vec<u8>> {
        let entry = self.load(chunk_id)?;
        Some(entry.chunk.owners.clone())
    }

    /// Does any tile in this chunk still belong to `owner`? Used by the loser
    /// side of a flip to decide whether to drop the chunk from its
    /// `owned_chunks` set.
    fn chunk_has_owner(&mut self, chunk_id: u64, owner: u32) -> bool {
        let entry = match self.load(chunk_id) {
            Some(e) => e,
            None => return false,
        };
        let byte = owner as u8;
        entry.chunk.owners.iter().any(|&o| o == byte)
    }

    /// Write every dirty chunk back to the DB exactly once.
    fn flush(self) {
        for (_id, entry) in self.entries {
            if entry.dirty {
                self.ctx.db.tile_chunks().id().update(entry.chunk);
            }
        }
    }
}

/// Add `chunk_id` to `player.owned_chunks` if not already present. Bounded.
fn player_add_chunk(player: &mut Player, chunk_id: u64) {
    if player.owned_chunks.len() >= OWNED_CHUNKS_CAP { return; }
    if !player.owned_chunks.contains(&chunk_id) {
        player.owned_chunks.push(chunk_id);
    }
}

/// Remove `chunk_id` from `player.owned_chunks` (no-op if absent).
fn player_remove_chunk(player: &mut Player, chunk_id: u64) {
    if let Some(pos) = player.owned_chunks.iter().position(|&c| c == chunk_id) {
        player.owned_chunks.swap_remove(pos);
    }
}

fn rng_next(seed: u64) -> u64 {
    seed.wrapping_mul(6364136223846793005).wrapping_add(1)
}

fn rng_range(seed: u64, max: u32) -> (u64, u32) {
    let next = rng_next(seed);
    (next, (next % (max as u64)) as u32)
}

// ── Tables ───────────────────────────────────────────────────────────────────

#[spacetimedb::table(name = matches, public)]
#[derive(Clone)]
pub struct Match {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    creator: Identity,
    name: String,
    tick: u64,
    phase: u8,
    created_at: Timestamp,
    winner: Option<u32>,
    total_land: u32,
    difficulty: u8,        // 0 Easy, 1 Normal, 2 Hard
    speed_multiplier: u8,  // Quarter-scale: 1 0.25×, 4 1×, 16 4×
    sub_tick_accum: u8,    // Remainder for sub-1× speeds
}

#[spacetimedb::table(name = players, public)]
#[derive(Clone)]
pub struct Player {
    #[primary_key]
    #[auto_inc]
    id: u32,
    #[index(btree)]
    match_id: u64,
    identity: Identity,
    name: String,
    color: u8,
    spawn_tile: Option<u32>,
    troops: f32,
    gold: f32,
    max_troops: f32,
    tiles: u32,
    alive: bool,
    is_bot: bool,
    traitor_until: u64,
    city_count: u32,
    city_levels: u32,
    // Chunk ids (full 64-bit form, `match_id<<32 | chunk_idx`) where this
    // player holds ≥1 tile. Maintained incrementally by ChunkCache::set_owner
    // on every tile claim and by the loser when a tile flips. Used by
    // `seed_attack_border` to find target-adjacent tiles without scanning
    // the whole 946-chunk world.
    owned_chunks: Vec<u64>,
    // Tick at which this player (really: bot) may issue its next attack.
    // Used by the simple bot AI to pace aggression. 0 = no pending cooldown.
    next_attack_tick: u64,
}

#[spacetimedb::table(name = tile_chunks, public)]
#[derive(Clone)]
pub struct TileChunk {
    #[primary_key]
    id: u64, // match_id << 32 | chunk_idx
    #[index(btree)]
    match_id: u64,
    chunk_x: u16,
    chunk_y: u16,
    owners: Vec<u8>,   // 255 = unclaimed/ocean
    terrain: Vec<u8>,  // 0=ocean, 1=plains, 2=highland, 3=mountain
}

#[spacetimedb::table(name = attacks, public)]
#[derive(Clone)]
pub struct Attack {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    match_id: u64,
    attacker: u32,
    // Target player id. 0 = wilderness (TerraNullius, per OpenFront).
    target: u32,
    // Live troop pool held by the attack. At launch this is deducted from
    // the attacker; each captured tile drains it via attack_logic. Retreat
    // refunds the remainder (minus RETREAT_MALUS for player targets).
    troops: f32,
    // Two-step retreat flag. retreat_attack sets this; step_match refunds
    // survivors + deletes the row next tick.
    retreating: bool,
    // Tiles owned by `target` that sit adjacent to an attacker tile. Kept
    // in sync by the conquest loop as new borders open up.
    border: Vec<u32>,
    // Parallel arrays that serialise a min-heap of tiles-to-conquer: lowest
    // priority value pops first. Reconstructed into a std BinaryHeap at
    // tick start and dumped back at tick end.
    to_conquer_tiles: Vec<u32>,
    to_conquer_priorities: Vec<i32>,
    // Starting tile for amphibious / boat attacks. None for ground attacks.
    // Reserved — unused in the MVP port.
    source_tile: Option<u32>,
    // Tick of the last border+heap refresh. Used to force a rebuild when
    // stale entries dominate.
    last_refresh_tick: u64,
}

#[spacetimedb::table(name = cities, public)]
#[derive(Clone)]
pub struct City {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    match_id: u64,
    owner_id: u32,
    tile: u32,
    level: u8,
    under_construction: bool,
    build_progress: u32,
}

#[spacetimedb::table(name = chat, public)]
#[derive(Clone)]
pub struct Chat {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    match_id: u64,
    from: u32,
    text: String,
    ts: u64,
}

#[spacetimedb::table(name = tick_schedule, scheduled(tick))]
#[derive(Clone)]
pub struct TickSchedule {
    #[primary_key]
    #[auto_inc]
    scheduled_id: u64,
    scheduled_at: ScheduleAt,
}

// ── Reducers ─────────────────────────────────────────────────────────────────

#[spacetimedb::reducer]
pub fn create_match(ctx: &ReducerContext, name: String, difficulty: u8) -> Result<(), String> {
    if difficulty > 2 {
        return Err("difficulty must be 0 (Easy), 1 (Normal), or 2 (Hard)".to_string());
    }
    let m = Match {
        id: 0,
        creator: ctx.sender,
        name,
        tick: 0,
        phase: PHASE_LOBBY,
        created_at: ctx.timestamp,
        winner: None,
        total_land: 0,
        difficulty,
        speed_multiplier: DEFAULT_SPEED,
        sub_tick_accum: 0,
    };
    ctx.db.matches().insert(m);
    Ok(())
}

#[spacetimedb::reducer]
pub fn leave_match(ctx: &ReducerContext, match_id: u64) -> Result<(), String> {
    let m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    let me = ctx.db.players().match_id().filter(match_id)
        .find(|p| p.identity == ctx.sender && !p.is_bot)
        .ok_or("Not in match")?;
    match m.phase {
        PHASE_LOBBY => {
            // Hard-delete the lobby player row — other players see them vanish.
            ctx.db.players().id().delete(me.id);
        }
        _ => {
            // In-flight match: flag the player dead, drop their troops. Their
            // tiles stay on the map and get absorbed by neighbours via the
            // normal passive-expansion / attack loop. Any open attacks they
            // launched clean themselves up in step_match when attacker.alive
            // is false.
            let mut me = me;
            me.alive = false;
            me.troops = 0.0;
            ctx.db.players().id().update(me);
        }
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn join_match(ctx: &ReducerContext, match_id: u64, name: String) -> Result<(), String> {
    let m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    if m.phase != PHASE_LOBBY {
        return Err("Match already started".to_string());
    }
    let count = ctx.db.players().match_id().filter(match_id).count() as u32;
    if count >= MAX_PLAYERS {
        return Err("Match full".to_string());
    }
    let color = count as u8;
    let p = Player {
        id: 0,
        match_id,
        identity: ctx.sender,
        name,
        color,
        spawn_tile: None,
        troops: 0.0,
        gold: 0.0,
        max_troops: 0.0,
        tiles: 0,
        alive: true,
        is_bot: false,
        traitor_until: 0,
        city_count: 0,
        city_levels: 0,
        owned_chunks: Vec::new(),
        next_attack_tick: 0,
    };
    ctx.db.players().insert(p);
    Ok(())
}

#[spacetimedb::reducer]
pub fn add_bot(ctx: &ReducerContext, match_id: u64, bot_name: String) -> Result<(), String> {
    let m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    if m.phase != PHASE_LOBBY {
        return Err("Match already started".to_string());
    }
    let count = ctx.db.players().match_id().filter(match_id).count() as u32;
    if count >= MAX_PLAYERS {
        return Err("Match full".to_string());
    }
    let color = count as u8;
    let p = Player {
        id: 0,
        match_id,
        identity: Identity::ZERO,
        name: bot_name,
        color,
        spawn_tile: None,
        troops: 0.0,
        gold: 0.0,
        max_troops: 0.0,
        tiles: 0,
        alive: true,
        is_bot: true,
        traitor_until: 0,
        city_count: 0,
        city_levels: 0,
        owned_chunks: Vec::new(),
        next_attack_tick: 0,
    };
    ctx.db.players().insert(p);
    Ok(())
}

#[spacetimedb::reducer]
pub fn set_speed(ctx: &ReducerContext, match_id: u64, speed_multiplier: u8) -> Result<(), String> {
    // Accept only {1, 2, 4, 8, 16} — matches the quarter-scale the UI exposes.
    if !matches!(speed_multiplier, 1 | 2 | 4 | 8 | 16) {
        return Err("speed_multiplier must be 1, 2, 4, 8, or 16".to_string());
    }
    let mut m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    // Any player in the match can change speed.
    let in_match = ctx.db.players().match_id().filter(match_id)
        .any(|p| p.identity == ctx.sender && !p.is_bot);
    if !in_match {
        return Err("Not in match".to_string());
    }
    m.speed_multiplier = speed_multiplier;
    m.sub_tick_accum = 0;
    ctx.db.matches().id().update(m);
    Ok(())
}

#[spacetimedb::reducer]
pub fn start_match(ctx: &ReducerContext, match_id: u64) -> Result<(), String> {
    let m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    if m.phase != PHASE_LOBBY {
        return Err("Already started".to_string());
    }
    generate_terrain(ctx, match_id)?;
    let mut m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    m.phase = PHASE_SPAWN;
    ctx.db.matches().id().update(m);
    Ok(())
}

fn generate_terrain(ctx: &ReducerContext, match_id: u64) -> Result<(), String> {
    let mut total_land = 0u32;
    for cy in 0..CHUNKS_Y {
        for cx in 0..CHUNKS_X {
            let mut owners = Vec::with_capacity((CHUNK_SIZE * CHUNK_SIZE) as usize);
            let mut terrain = Vec::with_capacity((CHUNK_SIZE * CHUNK_SIZE) as usize);
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let tx = cx * CHUNK_SIZE + lx;
                    let ty = cy * CHUNK_SIZE + ly;
                    // mask_terrain returns 0=ocean, 1=plains, 2=highland, 3=mountain.
                    // The ocean-mask bitfield and the terrain bitfield are baked from
                    // the same source and agree on which tiles are land.
                    let terrain_id = mask_terrain(tx, ty);
                    terrain.push(terrain_id);
                    owners.push(255);
                    if terrain_id != TERRAIN_OCEAN {
                        total_land += 1;
                    }
                }
            }
            let chunk = TileChunk {
                id: ((match_id as u64) << 32) | (chunk_idx(cx, cy) as u64),
                match_id,
                chunk_x: cx as u16,
                chunk_y: cy as u16,
                owners,
                terrain,
            };
            ctx.db.tile_chunks().insert(chunk);
        }
    }
    let mut m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    m.total_land = total_land;
    ctx.db.matches().id().update(m);
    Ok(())
}

#[spacetimedb::reducer]
pub fn spawn(ctx: &ReducerContext, match_id: u64, tile: u32) -> Result<(), String> {
    let mut m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    if m.phase != PHASE_SPAWN && m.phase != PHASE_PLAYING {
        return Err("Cannot spawn now".to_string());
    }
    let mut p = ctx.db.players()
        .match_id().filter(match_id)
        .find(|p| p.identity == ctx.sender && !p.is_bot)
        .ok_or("Not in match")?;
    if p.spawn_tile.is_some() {
        return Err("Already spawned".to_string());
    }
    let tx = tile % SIM_W;
    let ty = tile / SIM_W;
    if !is_land(ctx, match_id, tx, ty) {
        return Err("Must spawn on land".to_string());
    }
    for other in ctx.db.players().match_id().filter(match_id) {
        if let Some(ot) = other.spawn_tile {
            let ox = ot % SIM_W;
            let oy = ot / SIM_W;
            if dist_sq((tx, ty), (ox, oy)) < MIN_SPAWN_DIST * MIN_SPAWN_DIST {
                return Err("Too close to another player".to_string());
            }
        }
    }
    p.spawn_tile = Some(tile);
    let diff_idx = (m.difficulty as usize).min(2);
    p.troops = if p.is_bot {
        START_TROOPS_BOT
    } else {
        START_TROOPS_HUMAN * DIFF_HUMAN_TROOPS_MULT[diff_idx]
    };
    p.gold = 0.0;
    p.tiles = 1;
    p.max_troops = compute_max_troops(1, 0, p.is_bot);
    set_owner(ctx, match_id, tx, ty, p.id);
    if let Some(chunk_id) = spawn_chunk_id(match_id, tx, ty) {
        player_add_chunk(&mut p, chunk_id);
    }
    ctx.db.players().id().update(p);
    let all_spawned = ctx.db.players().match_id().filter(match_id).all(|p| p.spawn_tile.is_some());
    if all_spawned {
        m.phase = PHASE_PLAYING;
        ctx.db.matches().id().update(m);
    }
    Ok(())
}

#[spacetimedb::reducer]
pub fn bot_spawn(ctx: &ReducerContext, match_id: u64, player_id: u32, tile: u32) -> Result<(), String> {
    let mut p = ctx.db.players().id().find(player_id).ok_or("Player not found")?;
    if p.match_id != match_id || !p.is_bot {
        return Err("Invalid bot".to_string());
    }
    if p.spawn_tile.is_some() {
        return Err("Already spawned".to_string());
    }
    let tx = tile % SIM_W;
    let ty = tile / SIM_W;
    if !is_land(ctx, match_id, tx, ty) {
        return Err("Must spawn on land".to_string());
    }
    p.spawn_tile = Some(tile);
    p.troops = START_TROOPS_BOT;
    p.gold = 0.0;
    p.tiles = 1;
    p.max_troops = compute_max_troops(1, 0, p.is_bot);
    set_owner(ctx, match_id, tx, ty, player_id);
    ctx.db.players().id().update(p);
    Ok(())
}

#[spacetimedb::reducer]
pub fn launch_attack(ctx: &ReducerContext, match_id: u64, target_player: u32) -> Result<(), String> {
    let m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    if m.phase != PHASE_PLAYING {
        return Err("Match not in play".to_string());
    }
    let attacker_id = ctx.db.players()
        .match_id().filter(match_id)
        .find(|p| p.identity == ctx.sender && !p.is_bot)
        .ok_or("Not in match")?
        .id;
    let mut cache = ChunkCache::new(ctx, match_id);
    let result = launch_attack_internal(ctx, &mut cache, match_id, attacker_id, target_player, m.tick);
    cache.flush();
    result
}

// Shared core used by both the public reducer and bot AI. Ports OpenFront
// AttackExecution.init (AttackExecution.ts:100-169).
fn launch_attack_internal(
    ctx: &ReducerContext,
    cache: &mut ChunkCache,
    match_id: u64,
    attacker_id: u32,
    target_player: u32,
    current_tick: u64,
) -> Result<(), String> {
    if attacker_id == target_player { return Err("Cannot attack yourself".to_string()); }
    let mut attacker = ctx.db.players().id().find(attacker_id).ok_or("Attacker not found")?;
    if !attacker.alive { return Err("Eliminated".to_string()); }
    if attacker.spawn_tile.is_none() { return Err("Not yet spawned".to_string()); }

    if target_player != TARGET_WILDERNESS {
        let target = ctx.db.players().id().find(target_player).ok_or("Target not found")?;
        if target.match_id != match_id { return Err("Target not in match".to_string()); }
        if !target.alive { return Err("Target is dead".to_string()); }
    }

    let start = attack_amount(&attacker);
    if start < 1.0 { return Err("Not enough troops".to_string()); }
    attacker.troops -= start;
    ctx.db.players().id().update(attacker.clone());

    // Cancel-out: opposing attack from our target on us. Only meaningful for
    // player targets; wilderness has no attacks of its own.
    let mut my_pool = start;
    if target_player != TARGET_WILDERNESS {
        let opposing: Vec<_> = ctx.db.attacks().match_id().filter(match_id)
            .filter(|a| a.attacker == target_player && a.target == attacker_id && !a.retreating)
            .collect();
        for opp in opposing {
            if opp.troops > my_pool {
                let mut opp = opp;
                opp.troops -= my_pool;
                ctx.db.attacks().id().update(opp);
                return Ok(()); // fully absorbed, no new row
            } else {
                my_pool -= opp.troops;
                ctx.db.attacks().id().delete(opp.id);
                if my_pool < 1.0 { return Ok(()); }
            }
        }
    }

    // Merge with our existing attack on same target (§3d).
    let mine = ctx.db.attacks().match_id().filter(match_id)
        .find(|a| a.attacker == attacker_id && a.target == target_player && !a.retreating);
    if let Some(existing) = mine {
        let mut e = existing;
        e.troops += my_pool;
        ctx.db.attacks().id().update(e);
        return Ok(());
    }

    // Fresh attack. Seed border + heap from attacker's owned_chunks.
    let (border, heap_entries) = seed_attack_border(
        cache, &attacker, target_player, None, current_tick);
    if border.is_empty() {
        // Nothing to attack — refund and bail.
        attacker.troops += my_pool;
        ctx.db.players().id().update(attacker);
        return Err(
            if target_player == TARGET_WILDERNESS { "No bordering wilderness" }
            else { "No border with target" }.to_string());
    }
    let (to_conquer_tiles, to_conquer_priorities): (Vec<u32>, Vec<i32>) =
        heap_entries.into_iter().unzip();
    ctx.db.attacks().insert(Attack {
        id: 0,
        match_id,
        attacker: attacker_id,
        target: target_player,
        troops: my_pool,
        retreating: false,
        border,
        to_conquer_tiles,
        to_conquer_priorities,
        source_tile: None,
        last_refresh_tick: current_tick,
    });
    Ok(())
}

#[spacetimedb::reducer]
pub fn retreat_attack(ctx: &ReducerContext, match_id: u64, target_player: u32) -> Result<(), String> {
    let attacker = ctx.db.players()
        .match_id().filter(match_id)
        .find(|p| p.identity == ctx.sender && !p.is_bot)
        .ok_or("Not in match")?;
    let attack = ctx.db.attacks().match_id().filter(match_id)
        .find(|a| a.attacker == attacker.id && a.target == target_player && !a.retreating)
        .ok_or("No active attack")?;
    let mut attack = attack;
    attack.retreating = true;
    ctx.db.attacks().id().update(attack);
    Ok(())
}

#[spacetimedb::reducer]
pub fn build_city(ctx: &ReducerContext, match_id: u64, tile: u32) -> Result<(), String> {
    let m = ctx.db.matches().id().find(match_id).ok_or("Match not found")?;
    if m.phase != PHASE_PLAYING {
        return Err("Match not in play".to_string());
    }
    let mut p = ctx.db.players()
        .match_id().filter(match_id)
        .find(|p| p.identity == ctx.sender && !p.is_bot)
        .ok_or("Not in match")?;
    if !p.alive {
        return Err("Eliminated".to_string());
    }
    let tx = tile % SIM_W;
    let ty = tile / SIM_W;
    let owner = get_owner(ctx, match_id, tx, ty);
    if owner != Some(p.id) {
        return Err("Must build on your own tile".to_string());
    }
    let existing = ctx.db.cities().match_id().filter(match_id)
        .find(|c| c.tile == tile);
    if existing.is_some() {
        return Err("City already exists here".to_string());
    }
    let cost = city_cost(p.city_count);
    if p.gold < cost {
        return Err("Not enough gold".to_string());
    }
    let owner_id = p.id;
    p.gold -= cost;
    p.city_count += 1;
    p.city_levels += 1;
    p.max_troops = compute_max_troops(p.tiles, p.city_levels, p.is_bot);
    ctx.db.players().id().update(p);
    let city = City {
        id: 0,
        match_id,
        owner_id,
        tile,
        level: 1,
        under_construction: false,
        build_progress: 0,
    };
    ctx.db.cities().insert(city);
    Ok(())
}

#[spacetimedb::reducer]
pub fn send_chat(ctx: &ReducerContext, match_id: u64, text: String) -> Result<(), String> {
    let p = ctx.db.players()
        .match_id().filter(match_id)
        .find(|p| p.identity == ctx.sender)
        .ok_or("Not in match")?;
    let chat = Chat {
        id: 0,
        match_id,
        from: p.id,
        text,
        ts: ctx.timestamp.to_micros_since_unix_epoch() as u64 / 1000,
    };
    ctx.db.chat().insert(chat);
    Ok(())
}

// ── Scheduled Tick ───────────────────────────────────────────────────────────

#[spacetimedb::reducer(init)]
pub fn init(ctx: &ReducerContext) {
    let loop_duration = TimeDuration::from_micros((TICK_MS * 1000) as i64);
    ctx.db.tick_schedule().insert(TickSchedule {
        scheduled_id: 0,
        scheduled_at: loop_duration.into(),
    });
}

#[spacetimedb::reducer]
pub fn tick(ctx: &ReducerContext, _schedule: TickSchedule) {
    let active_matches: Vec<_> = ctx.db.matches().iter()
        .filter(|m| m.phase == PHASE_PLAYING || m.phase == PHASE_SPAWN)
        .collect();
    for mut m in active_matches {
        let count = sub_tick_count(&mut m);
        for _ in 0..count {
            step_match(ctx, &mut m);
            if m.phase == PHASE_ENDED { break; }
        }
    }
}

// Given a match's quarter-scale speed_multiplier (1, 2, 4, 8, 16 → 0.25×, 0.5×,
// 1×, 2×, 4×), return how many full step_match() passes to run this real tick.
// For sub-1× speeds we accumulate sub_tick_accum and only step when it wraps.
fn sub_tick_count(m: &mut Match) -> u32 {
    let speed = m.speed_multiplier.clamp(1, 16) as u32;
    if speed >= 4 {
        // 1×, 2×, 4× → integer multiple of steps per real tick.
        return speed / 4;
    }
    // 0.25× or 0.5× → accumulate and step every N real ticks.
    // Step when accum + speed >= 4; subtract 4 and keep remainder.
    m.sub_tick_accum = m.sub_tick_accum.saturating_add(speed as u8);
    if m.sub_tick_accum >= 4 {
        m.sub_tick_accum -= 4;
        1
    } else {
        0
    }
}

fn step_match(ctx: &ReducerContext, m: &mut Match) {
    m.tick += 1;
    let match_id = m.id;

    // Per-tick chunk cache. All tile reads/writes below flow through this so
    // repeated access to the same chunk incurs one DB read and one DB write.
    let mut cache = ChunkCache::new(ctx, match_id);

    // ── 1. Regen troops & passive gold ───────────────────────────────────────
    let players: Vec<_> = ctx.db.players().match_id().filter(match_id).collect();
    for mut p in players {
        if !p.alive || p.spawn_tile.is_none() {
            continue;
        }
        p.max_troops = compute_max_troops(p.tiles, p.city_levels, p.is_bot);
        let regen = regen_rate(p.troops, p.max_troops, p.is_bot);
        p.troops = (p.troops + regen).min(p.max_troops);
        let gold_income = if p.is_bot { GOLD_PER_TICK_BOT } else { GOLD_PER_TICK_HUMAN };
        p.gold += gold_income;
        ctx.db.players().id().update(p);
    }

    // ── 2. Attack tick: heap-based conquest + retreat refund ─────────────────
    let attacks: Vec<_> = ctx.db.attacks().match_id().filter(match_id).collect();
    for attack in attacks {
        process_attack(ctx, &mut cache, match_id, m.tick, attack);
    }

    // ── 3. Bot AI — issue periodic attacks on cooldown ───────────────────────
    if m.phase == PHASE_PLAYING {
        bot_ai_tick(ctx, &mut cache, match_id, m.tick);
    }

    // ── 4. Auto-spawn bots during SPAWN phase ────────────────────────────────
    let unsp_bots: Vec<_> = ctx.db.players().match_id().filter(match_id)
        .filter(|p| p.is_bot && p.alive && p.spawn_tile.is_none())
        .collect();
    if m.phase == PHASE_SPAWN {
        for p in unsp_bots {
            let seed = rng_seed(match_id, m.tick, p.id as u64);
            if let Some((tx, ty)) = find_bot_spawn(ctx, match_id, seed) {
                let _ = bot_spawn_internal(ctx, match_id, p.id, tile_idx(tx, ty));
            }
        }
    }

    // ── 5. Check win ─────────────────────────────────────────────────────────
    if m.phase == PHASE_PLAYING && m.total_land > 0 {
        let players_check: Vec<_> = ctx.db.players().match_id().filter(match_id).collect();
        for p in players_check {
            if !p.alive { continue; }
            let pct = p.tiles as f32 / m.total_land as f32;
            if pct >= WIN_PCT {
                m.winner = Some(p.id);
                m.phase = PHASE_ENDED;
                break;
            }
        }
    }

    ctx.db.matches().id().update(m.clone());

    // Write every dirty chunk back to the DB — exactly once per chunk,
    // regardless of how many tile edits landed on it this tick.
    cache.flush();
}

// ── Tile helpers ─────────────────────────────────────────────────────────────

fn is_land(ctx: &ReducerContext, match_id: u64, tx: u32, ty: u32) -> bool {
    if tx >= SIM_W || ty >= SIM_H { return false; }
    let (cx, cy, lx, ly) = tile_to_chunk(tx, ty);
    let chunk_id = ((match_id as u64) << 32) | (chunk_idx(cx, cy) as u64);
    match ctx.db.tile_chunks().id().find(chunk_id) {
        Some(chunk) => {
            let idx = (ly * CHUNK_SIZE + lx) as usize;
            // Any non-ocean terrain code counts as land (plains/highland/mountain).
            chunk.terrain.get(idx).copied().unwrap_or(0) != 0
        }
        None => false,
    }
}

fn get_owner(ctx: &ReducerContext, match_id: u64, tx: u32, ty: u32) -> Option<u32> {
    if tx >= SIM_W || ty >= SIM_H { return None; }
    let (cx, cy, lx, ly) = tile_to_chunk(tx, ty);
    let chunk_id = ((match_id as u64) << 32) | (chunk_idx(cx, cy) as u64);
    match ctx.db.tile_chunks().id().find(chunk_id) {
        Some(chunk) => {
            let idx = (ly * CHUNK_SIZE + lx) as usize;
            match chunk.owners.get(idx).copied() {
                Some(255) | None => None,
                Some(o) => Some(o as u32),
            }
        }
        None => None,
    }
}

fn set_owner(ctx: &ReducerContext, match_id: u64, tx: u32, ty: u32, owner: u32) {
    if tx >= SIM_W || ty >= SIM_H { return; }
    let (cx, cy, lx, ly) = tile_to_chunk(tx, ty);
    let chunk_id = ((match_id as u64) << 32) | (chunk_idx(cx, cy) as u64);
    if let Some(mut chunk) = ctx.db.tile_chunks().id().find(chunk_id) {
        let idx = (ly * CHUNK_SIZE + lx) as usize;
        if idx < chunk.owners.len() {
            chunk.owners[idx] = owner as u8;
            ctx.db.tile_chunks().id().update(chunk);
        }
    }
}

// ── Attack tick: per-attack heap-based conquest ─────────────────────────────

fn process_attack(
    ctx: &ReducerContext,
    cache: &mut ChunkCache,
    match_id: u64,
    current_tick: u64,
    mut attack: Attack,
) {
    // Retreat: refund survivors (75% for player targets, 100% for wilderness)
    // and delete the attack row.
    if attack.retreating {
        if let Some(mut attacker) = ctx.db.players().id().find(attack.attacker) {
            let refund_mult = if attack.target == TARGET_WILDERNESS {
                1.0
            } else {
                1.0 - RETREAT_MALUS
            };
            attacker.troops += attack.troops * refund_mult;
            ctx.db.players().id().update(attacker);
        }
        ctx.db.attacks().id().delete(attack.id);
        return;
    }

    let mut attacker = match ctx.db.players().id().find(attack.attacker) {
        Some(p) if p.alive && p.spawn_tile.is_some() => p,
        _ => { ctx.db.attacks().id().delete(attack.id); return; }
    };
    let mut target: Option<Player> = if attack.target == TARGET_WILDERNESS {
        None
    } else {
        match ctx.db.players().id().find(attack.target) {
            Some(p) if p.alive => Some(p),
            _ => { ctx.db.attacks().id().delete(attack.id); return; }
        }
    };

    // Reconstruct min-heap from the parallel Vecs persisted on the row.
    let mut heap: BinaryHeap<Reverse<(i32, u32)>> = BinaryHeap::new();
    for (i, &tile) in attack.to_conquer_tiles.iter().enumerate() {
        let prio = attack.to_conquer_priorities.get(i).copied().unwrap_or(0);
        heap.push(Reverse((prio, tile)));
    }

    let jitter = (rng_seed(match_id, current_tick, attack.id) % 5) as u32;
    let mut tiles_per_tick = attack_tiles_per_tick(
        attack.troops, target.as_ref(), attack.border.len() as u32 + jitter);

    // Dirty-set to short-circuit border.contains() while we push neighbours.
    let mut border_set: std::collections::HashSet<u32> =
        attack.border.iter().copied().collect();

    let mut deleted = false;
    while tiles_per_tick > 0.0 {
        if attack.troops < 1.0 {
            ctx.db.attacks().id().delete(attack.id);
            deleted = true;
            break;
        }
        let entry = heap.pop();
        let tile = match entry {
            Some(Reverse((_prio, t))) => t,
            None => {
                // Heap drained — refresh from the full attacker border.
                let (new_border, new_heap) = seed_attack_border(
                    cache, &attacker, attack.target, attack.source_tile, current_tick);
                if new_heap.is_empty() {
                    // Nothing left to conquer — refund + delete (auto-retreat).
                    let refund_mult = if attack.target == TARGET_WILDERNESS {
                        1.0
                    } else {
                        1.0 - RETREAT_MALUS
                    };
                    attacker.troops += attack.troops * refund_mult;
                    ctx.db.attacks().id().delete(attack.id);
                    deleted = true;
                    break;
                }
                border_set = new_border.iter().copied().collect();
                attack.border = new_border;
                for (t, p) in &new_heap {
                    heap.push(Reverse((*p, *t)));
                }
                attack.last_refresh_tick = current_tick;
                continue;
            }
        };
        let tx = tile % SIM_W;
        let ty = tile / SIM_W;

        // Stale check: tile may have flipped since it was enqueued.
        if !is_capturable(attack.target, cache.get_owner(tx, ty)) { continue; }
        if !has_attacker_neighbor(cache, attacker.id, tx, ty) { continue; }

        // Combat on this tile.
        let terrain = cache.terrain_at(tx, ty);
        let step = attack_logic(attack.troops, &attacker, target.as_ref(), terrain);
        tiles_per_tick -= step.tiles_used;
        attack.troops = (attack.troops - step.attacker_loss).max(0.0);
        if let Some(ref mut t) = target {
            t.troops = (t.troops - step.defender_loss).max(0.0);
        }

        // Guarded flip — only count the capture if prev_owner matched our target.
        if let Some((chunk_id, prev_owner)) = cache.set_owner(tx, ty, attacker.id) {
            if is_capturable(attack.target, prev_owner) {
                player_add_chunk(&mut attacker, chunk_id);
                attacker.tiles += 1;
                if let Some(ref mut t) = target {
                    if t.tiles > 0 { t.tiles -= 1; }
                    if !cache.chunk_has_owner(chunk_id, t.id) {
                        player_remove_chunk(t, chunk_id);
                    }
                    if t.tiles < DEFENDER_DEAD_TILES_THRESH && t.alive {
                        t.alive = false;
                    }
                }
                // Enqueue the captured tile's outward neighbours so the front
                // keeps advancing.
                let pushed = enqueue_tile_neighbors(
                    cache, attacker.id, attack.target, tx, ty, current_tick);
                for (ntile, nprio) in pushed {
                    if border_set.insert(ntile) {
                        if attack.border.len() < ATTACK_HEAP_CAP {
                            attack.border.push(ntile);
                        }
                        heap.push(Reverse((nprio, ntile)));
                    }
                }
            }
        }

        if attack.troops < 1.0 {
            ctx.db.attacks().id().delete(attack.id);
            deleted = true;
            break;
        }
    }

    if !deleted {
        attack.to_conquer_tiles.clear();
        attack.to_conquer_priorities.clear();
        for Reverse((p, t)) in heap {
            attack.to_conquer_tiles.push(t);
            attack.to_conquer_priorities.push(p);
            if attack.to_conquer_tiles.len() >= ATTACK_HEAP_CAP { break; }
        }
        ctx.db.attacks().id().update(attack);
    }
    ctx.db.players().id().update(attacker);
    if let Some(t) = target {
        ctx.db.players().id().update(t);
    }
}

// ── Bot AI: periodic attack issuance (TribeExecution-lite) ──────────────────

fn bot_ai_tick(ctx: &ReducerContext, cache: &mut ChunkCache, match_id: u64, current_tick: u64) {
    let active_bots: Vec<_> = ctx.db.players().match_id().filter(match_id)
        .filter(|p| p.is_bot && p.alive && p.spawn_tile.is_some())
        .collect();
    for bot in active_bots {
        if bot.next_attack_tick > current_tick { continue; }
        // Skip if we already have an active attack.
        let busy = ctx.db.attacks().match_id().filter(match_id)
            .any(|a| a.attacker == bot.id && !a.retreating);
        if busy {
            continue;
        }

        let pick = pick_bot_target(cache, &bot);
        if let Some(target_id) = pick {
            let _ = launch_attack_internal(ctx, cache, match_id, bot.id, target_id, current_tick);
        }
        // Schedule next attempt regardless of success.
        let seed = rng_seed(match_id, current_tick, bot.id as u64);
        let range = (BOT_ATTACK_COOLDOWN_MAX_TICKS - BOT_ATTACK_COOLDOWN_MIN_TICKS).max(1);
        let offset = seed % range;
        if let Some(mut bot) = ctx.db.players().id().find(bot.id) {
            bot.next_attack_tick = current_tick + BOT_ATTACK_COOLDOWN_MIN_TICKS + offset;
            ctx.db.players().id().update(bot);
        }
    }
}

fn pick_bot_target(cache: &mut ChunkCache, bot: &Player) -> Option<u32> {
    let mut neighbours: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut has_wilderness = false;
    let attacker_byte = bot.id as u8;
    let chunk_ids: Vec<u64> = bot.owned_chunks.clone();
    let dirs = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)];
    'outer: for chunk_id in chunk_ids {
        let owners = match cache.clone_owners(chunk_id) {
            Some(o) => o,
            None => continue,
        };
        let local_idx = (chunk_id & 0xFFFF_FFFF) as u32;
        let cx = local_idx % CHUNKS_X;
        let cy = local_idx / CHUNKS_X;
        for ly in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let idx = (ly * CHUNK_SIZE + lx) as usize;
                if owners.get(idx).copied().unwrap_or(255) != attacker_byte { continue; }
                let tx = cx * CHUNK_SIZE + lx;
                let ty = cy * CHUNK_SIZE + ly;
                if tx >= SIM_W || ty >= SIM_H { continue; }
                for (dx, dy) in dirs {
                    let nx = tx as i32 + dx;
                    let ny = ty as i32 + dy;
                    if nx < 0 || nx >= SIM_W as i32 || ny < 0 || ny >= SIM_H as i32 { continue; }
                    let nxu = nx as u32;
                    let nyu = ny as u32;
                    if !cache.is_land(nxu, nyu) { continue; }
                    match cache.get_owner(nxu, nyu) {
                        None => has_wilderness = true,
                        Some(o) if o != bot.id => { neighbours.insert(o); }
                        _ => {}
                    }
                }
                if neighbours.len() >= 4 && has_wilderness { break 'outer; }
            }
        }
    }
    let mut options: Vec<u32> = neighbours.into_iter().collect();
    if has_wilderness { options.push(TARGET_WILDERNESS); }
    if options.is_empty() { return None; }
    let seed = rng_seed(bot.match_id, 0, bot.id as u64);
    let idx = (seed as usize) % options.len();
    Some(options[idx])
}

// ── Bot AI ───────────────────────────────────────────────────────────────────

fn find_bot_spawn(ctx: &ReducerContext, match_id: u64, seed: u64) -> Option<(u32, u32)> {
    let mut s = seed;
    for _ in 0..2000 {
        let (ns, rx) = rng_range(s, SIM_W);
        let (ns, ry) = rng_range(ns, SIM_H);
        s = ns;
        if !is_land(ctx, match_id, rx, ry) { continue; }
        let mut ok = true;
        for p in ctx.db.players().match_id().filter(match_id) {
            if let Some(st) = p.spawn_tile {
                let ox = st % SIM_W;
                let oy = st / SIM_W;
                if dist_sq((rx, ry), (ox, oy)) < MIN_SPAWN_DIST * MIN_SPAWN_DIST {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some((rx, ry));
        }
    }
    None
}

fn bot_spawn_internal(ctx: &ReducerContext, match_id: u64, player_id: u32, tile: u32) -> Result<(), String> {
    let mut p = ctx.db.players().id().find(player_id).ok_or("Player not found")?;
    if p.match_id != match_id || !p.is_bot {
        return Err("Invalid bot".to_string());
    }
    if p.spawn_tile.is_some() {
        return Err("Already spawned".to_string());
    }
    let tx = tile % SIM_W;
    let ty = tile / SIM_W;
    if !is_land(ctx, match_id, tx, ty) {
        return Err("Must spawn on land".to_string());
    }
    p.spawn_tile = Some(tile);
    p.troops = START_TROOPS_BOT;
    p.gold = 0.0;
    p.tiles = 1;
    p.max_troops = compute_max_troops(1, 0, p.is_bot);
    set_owner(ctx, match_id, tx, ty, player_id);
    // Track the spawn tile's chunk so seed_attack_border finds the bot.
    if let Some(chunk_id) = spawn_chunk_id(match_id, tx, ty) {
        player_add_chunk(&mut p, chunk_id);
    }
    ctx.db.players().id().update(p);
    Ok(())
}

// Small helper used by one-shot reducers (spawn, bot_spawn_internal, build_city)
// that don't use a ChunkCache. Returns the full 64-bit chunk id for a tile.
fn spawn_chunk_id(match_id: u64, tx: u32, ty: u32) -> Option<u64> {
    if tx >= SIM_W || ty >= SIM_H { return None; }
    let cx = tx / CHUNK_SIZE;
    let cy = ty / CHUNK_SIZE;
    Some((match_id << 32) | (chunk_idx(cx, cy) as u64))
}
