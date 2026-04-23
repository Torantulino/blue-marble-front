use spacetimedb::{ReducerContext, Table, Timestamp, Identity, TimeDuration, ScheduleAt};

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
const DIFF_HUMAN_TROOPS_MULT: [f32; 3] = [1.4, 1.0, 0.7];
const DIFF_BOT_EXPAND_STEPS: [u32; 3] = [30, 50, 75];
const HUMAN_EXPAND_STEPS: u32 = 40; // Matches M0's stepsForPlayer for the human.

// Speed multiplier (stored on Match.speed_multiplier, quarter-scale):
// 1=0.25×, 2=0.5×, 4=1×, 8=2×, 16=4×. See sub_tick_count().
const DEFAULT_SPEED: u8 = 4;

// Cap on how many frontier tiles per player we persist (oldest overflow spills).
const FRONTIER_CAP: usize = 4096;

// NASA ocean-mask bitfield. 1350×675 bits = 113907 bytes. Baked from the NASA
// oceanmask PNG by scripts/bake-ocean-mask.mjs. 1 bit = 1 sim tile, row-major;
// 1 = land, 0 = ocean.
const OCEAN_MASK: &[u8] = include_bytes!("../assets/ocean_mask_1350x675.bin");

fn mask_is_land(tx: u32, ty: u32) -> bool {
    if tx >= SIM_W || ty >= SIM_H { return false; }
    let bit = ty * SIM_W + tx;
    let byte = (bit >> 3) as usize;
    let shift = (bit & 7) as u8;
    (OCEAN_MASK[byte] >> shift) & 1 == 1
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

fn compute_max_troops(tiles: u32, city_levels: u32) -> f32 {
    let t = tiles as f32;
    2.0 * (t.powf(0.6) * 1000.0 + 50000.0) + (city_levels as f32) * CITY_TROOP_BONUS
}

fn regen_rate(troops: f32, max_troops: f32) -> f32 {
    let base = 10.0 + troops.powf(0.73) / 4.0;
    let taper = 1.0 - (troops / max_troops).min(1.0);
    base * taper.max(0.0)
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
    // Tiles adjacent to our territory waiting to be claimed or contested.
    // Row-major tile indices. Capped at FRONTIER_CAP. May contain stale
    // entries (no longer land / already mine / now enemy-owned); passive
    // expansion filters at pop time.
    frontier_tiles: Vec<u32>,
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
    terrain: Vec<u8>,  // 1 = land, 0 = ocean
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
    target: u32,
    troops_committed: f32,
    retreating: bool,
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
            me.frontier_tiles.clear();
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
        frontier_tiles: Vec::new(),
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
        frontier_tiles: Vec::new(),
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
                    let is_land = mask_is_land(tx, ty);
                    terrain.push(if is_land { 1 } else { 0 });
                    owners.push(255);
                    if is_land {
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
    p.max_troops = compute_max_troops(1, 0);
    set_owner(ctx, match_id, tx, ty, p.id);
    seed_frontier(&mut p, tx, ty);
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
    p.max_troops = compute_max_troops(1, 0);
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
    let attacker = ctx.db.players()
        .match_id().filter(match_id)
        .find(|p| p.identity == ctx.sender && !p.is_bot)
        .ok_or("Not in match")?;
    if !attacker.alive {
        return Err("Eliminated".to_string());
    }
    let target = ctx.db.players().id().find(target_player).ok_or("Target not found")?;
    if target.match_id != match_id || !target.alive {
        return Err("Invalid target".to_string());
    }
    let existing = ctx.db.attacks().match_id().filter(match_id)
        .find(|a| a.attacker == attacker.id && a.target == target_player);
    if existing.is_some() {
        return Err("Attack already active".to_string());
    }
    let commit = attacker.troops / 5.0;
    let a = Attack {
        id: 0,
        match_id,
        attacker: attacker.id,
        target: target_player,
        troops_committed: commit,
        retreating: false,
    };
    ctx.db.attacks().insert(a);
    Ok(())
}

#[spacetimedb::reducer]
pub fn retreat_attack(ctx: &ReducerContext, match_id: u64, target_player: u32) -> Result<(), String> {
    let attacker = ctx.db.players()
        .match_id().filter(match_id)
        .find(|p| p.identity == ctx.sender && !p.is_bot)
        .ok_or("Not in match")?;
    let attack = ctx.db.attacks().match_id().filter(match_id)
        .find(|a| a.attacker == attacker.id && a.target == target_player)
        .ok_or("No active attack")?;
    ctx.db.attacks().id().delete(attack.id);
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
    p.max_troops = compute_max_troops(p.tiles, p.city_levels);
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
    let diff_idx = (m.difficulty as usize).min(2);

    // ── 1. Regen troops & passive gold ───────────────────────────────────────
    let players: Vec<_> = ctx.db.players().match_id().filter(match_id).collect();
    for mut p in players {
        if !p.alive || p.spawn_tile.is_none() {
            continue;
        }
        p.max_troops = compute_max_troops(p.tiles, p.city_levels);
        let regen = regen_rate(p.troops, p.max_troops);
        p.troops = (p.troops + regen).min(p.max_troops);
        let gold_income = if p.is_bot { GOLD_PER_TICK_BOT } else { GOLD_PER_TICK_HUMAN };
        p.gold += gold_income;
        ctx.db.players().id().update(p);
    }

    // ── 2. Directed attacks ──────────────────────────────────────────────────
    let attacks: Vec<_> = ctx.db.attacks().match_id().filter(match_id).collect();
    for attack in attacks {
        if attack.retreating {
            ctx.db.attacks().id().delete(attack.id);
            continue;
        }
        let mut attacker = match ctx.db.players().id().find(attack.attacker) {
            Some(p) => p,
            None => { ctx.db.attacks().id().delete(attack.id); continue; }
        };
        let mut target = match ctx.db.players().id().find(attack.target) {
            Some(p) => p,
            None => { ctx.db.attacks().id().delete(attack.id); continue; }
        };
        if !attacker.alive || !target.alive {
            ctx.db.attacks().id().delete(attack.id);
            continue;
        }

        let border = find_border_tiles(ctx, match_id, attacker.id, target.id);
        if border.is_empty() {
            ctx.db.attacks().id().delete(attack.id);
            continue;
        }

        let atk_str = attack.troops_committed * 0.25;
        let def_str = target.troops * 0.3;
        let max_take = ((border.len() as f32) * 0.3).ceil() as usize;

        for (tx, ty) in border.into_iter().take(max_take) {
            if attacker.troops < 10.0 || target.troops < 1.0 {
                break;
            }
            if atk_str > def_str + 10.0 {
                attacker.troops -= def_str * 0.05;
                target.troops -= atk_str * 0.04;
                if target.troops < 1.0 { target.troops = 1.0; }
                set_owner(ctx, match_id, tx, ty, attacker.id);
                attacker.tiles += 1;
                target.tiles -= 1;
                add_frontier_neighbors(&mut attacker, tx, ty);
            } else {
                attacker.troops -= atk_str * 0.02;
            }
            if attacker.troops < 10.0 { break; }
        }

        ctx.db.players().id().update(attacker.clone());
        ctx.db.players().id().update(target.clone());

        if target.tiles == 0 {
            target.alive = false;
            ctx.db.players().id().update(target);
            ctx.db.attacks().id().delete(attack.id);
        }
    }

    // ── 3. Passive expansion for every alive player (human and bot) ──────────
    let active_players: Vec<_> = ctx.db.players().match_id().filter(match_id)
        .filter(|p| p.alive && p.spawn_tile.is_some())
        .collect();
    for mut p in active_players {
        let steps = if p.is_bot { DIFF_BOT_EXPAND_STEPS[diff_idx] } else { HUMAN_EXPAND_STEPS };
        let seed = rng_seed(match_id, m.tick, p.id as u64);
        passive_expand(ctx, match_id, &mut p, steps, seed);
        ctx.db.players().id().update(p);
    }

    // ── 4. Bot directed-attack AI ────────────────────────────────────────────
    let bot_attackers: Vec<_> = ctx.db.players().match_id().filter(match_id)
        .filter(|p| p.is_bot && p.alive && p.spawn_tile.is_some())
        .collect();
    if m.phase == PHASE_PLAYING {
        for p in bot_attackers {
            bot_attack(ctx, match_id, &p);
        }
    }

    // ── 5. Auto-spawn bots during SPAWN phase ────────────────────────────────
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

    // ── 6. Check win ─────────────────────────────────────────────────────────
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
}

// ── Passive expansion / frontier helpers ────────────────────────────────────

fn seed_frontier(p: &mut Player, tx: u32, ty: u32) {
    p.frontier_tiles.clear();
    add_frontier_neighbors(p, tx, ty);
}

fn add_frontier_neighbors(p: &mut Player, tx: u32, ty: u32) {
    let dirs = [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)];
    for (dx, dy) in dirs {
        let nx = tx as i32 + dx;
        let ny = ty as i32 + dy;
        if nx < 0 || nx >= SIM_W as i32 || ny < 0 || ny >= SIM_H as i32 { continue; }
        let nxu = nx as u32;
        let nyu = ny as u32;
        if !mask_is_land(nxu, nyu) { continue; }
        let nidx = nyu * SIM_W + nxu;
        if p.frontier_tiles.len() >= FRONTIER_CAP { continue; }
        p.frontier_tiles.push(nidx);
    }
}

fn passive_expand(ctx: &ReducerContext, match_id: u64, p: &mut Player, steps: u32, seed: u64) {
    if p.frontier_tiles.is_empty() { return; }
    let mut s = seed;
    for _ in 0..steps {
        if p.frontier_tiles.is_empty() { break; }
        let (ns, ri) = rng_range(s, p.frontier_tiles.len() as u32);
        s = ns;
        let tile = p.frontier_tiles.swap_remove(ri as usize);
        let tx = tile % SIM_W;
        let ty = tile / SIM_W;
        if !is_land(ctx, match_id, tx, ty) { continue; }
        match get_owner(ctx, match_id, tx, ty) {
            Some(o) if o == p.id => continue, // stale — already ours
            None => {
                // Claim unclaimed land.
                set_owner(ctx, match_id, tx, ty, p.id);
                p.tiles += 1;
                p.troops = (p.troops - 1.0).max(1.0);
                add_frontier_neighbors(p, tx, ty);
            }
            Some(target_id) => {
                // Border skirmish with adjacent enemy tile.
                let mut target = match ctx.db.players().id().find(target_id) {
                    Some(t) => t,
                    None => continue,
                };
                if !target.alive { continue; }
                let atk_str = p.troops * 0.25;
                let def_str = target.troops * 0.3;
                if atk_str > def_str + 10.0 {
                    p.troops -= def_str * 0.05;
                    target.troops -= atk_str * 0.04;
                    if target.troops < 1.0 { target.troops = 1.0; }
                    set_owner(ctx, match_id, tx, ty, p.id);
                    p.tiles += 1;
                    if target.tiles > 0 { target.tiles -= 1; }
                    add_frontier_neighbors(p, tx, ty);
                    if target.tiles == 0 {
                        target.alive = false;
                    }
                } else {
                    p.troops -= atk_str * 0.02;
                }
                ctx.db.players().id().update(target);
            }
        }
        if p.troops < 10.0 { break; }
    }
    p.max_troops = compute_max_troops(p.tiles, p.city_levels);
}

// ── Tile helpers ─────────────────────────────────────────────────────────────

fn is_land(ctx: &ReducerContext, match_id: u64, tx: u32, ty: u32) -> bool {
    if tx >= SIM_W || ty >= SIM_H { return false; }
    let (cx, cy, lx, ly) = tile_to_chunk(tx, ty);
    let chunk_id = ((match_id as u64) << 32) | (chunk_idx(cx, cy) as u64);
    match ctx.db.tile_chunks().id().find(chunk_id) {
        Some(chunk) => {
            let idx = (ly * CHUNK_SIZE + lx) as usize;
            chunk.terrain.get(idx).copied().unwrap_or(0) == 1
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

fn find_border_tiles(ctx: &ReducerContext, match_id: u64, a: u32, b: u32) -> Vec<(u32, u32)> {
    let mut border = Vec::new();
    for cy in 0..CHUNKS_Y {
        for cx in 0..CHUNKS_X {
            let chunk_id = ((match_id as u64) << 32) | (chunk_idx(cx, cy) as u64);
            if let Some(chunk) = ctx.db.tile_chunks().id().find(chunk_id) {
                for ly in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        let tx = cx * CHUNK_SIZE + lx;
                        let ty = cy * CHUNK_SIZE + ly;
                        if tx >= SIM_W || ty >= SIM_H { continue; }
                        let idx = (ly * CHUNK_SIZE + lx) as usize;
                        if chunk.owners.get(idx).copied().unwrap_or(255) as u32 != a {
                            continue;
                        }
                        let dirs = [(1i32,0i32), (-1,0), (0,1), (0,-1)];
                        for (dx, dy) in dirs {
                            let nx = (tx as i32 + dx) as u32;
                            let ny = (ty as i32 + dy) as u32;
                            if nx >= SIM_W || ny >= SIM_H { continue; }
                            if get_owner(ctx, match_id, nx, ny) == Some(b) {
                                border.push((tx, ty));
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    border
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
    p.max_troops = compute_max_troops(1, 0);
    set_owner(ctx, match_id, tx, ty, player_id);
    seed_frontier(&mut p, tx, ty);
    ctx.db.players().id().update(p);
    Ok(())
}

fn bot_attack(ctx: &ReducerContext, match_id: u64, p: &Player) {
    let mut nearest: Option<u32> = None;
    let mut best_dist = u32::MAX;
    let (px, py) = match p.spawn_tile {
        Some(t) => (t % SIM_W, t / SIM_W),
        None => return,
    };
    for other in ctx.db.players().match_id().filter(match_id) {
        if other.id == p.id || !other.alive { continue; }
        if let Some(ot) = other.spawn_tile {
            let ox = ot % SIM_W;
            let oy = ot / SIM_W;
            let d = dist_sq((px, py), (ox, oy));
            if d < best_dist {
                best_dist = d;
                nearest = Some(other.id);
            }
        }
    }
    if let Some(target_id) = nearest {
        let existing = ctx.db.attacks().match_id().filter(match_id)
            .find(|a| a.attacker == p.id && a.target == target_id);
        if existing.is_none() {
            let commit = p.troops / 20.0;
            let a = Attack {
                id: 0,
                match_id,
                attacker: p.id,
                target: target_id,
                troops_committed: commit,
                retreating: false,
            };
            ctx.db.attacks().insert(a);
        }
    }
}
