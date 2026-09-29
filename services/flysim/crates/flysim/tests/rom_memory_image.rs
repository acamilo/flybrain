//! MEM-01: the macro engine over a boundary's memory image is the macro engine over the live
//! emulator, frame by frame, on the real cartridge.
//!
//! Gated on `FLY_ROM` and the checkpoints `the operator's rom env script` exports; every
//! scenario whose checkpoint is not set is skipped, and without `FLY_ROM` the whole test is:
//!
//! ```sh
//! . the operator's rom env script
//! cargo test --release -p flysim --test rom_memory_image -- --nocapture
//! ```
//!
//! ## What runs
//!
//! Two copies of the Pokémon task and executor over one cartridge. The **live** arm is the
//! stream's own frame (`LegacyFrame::execute` and `stub_advance`, the stub-readout drivers' path):
//! its `MacroLayer` and `PokemonRedReward` read the emulator through `MemoryReader`, and its mask
//! is the one the emulator is given. The **shadow** arm is what the session framework's
//! `pokered-macros-v1` executor will be (`legacy-gameboy-v1` sections 8 and 10): a second
//! `MacroLayer` and a second `PokemonRedReward`, built by the same constructors, that never see the
//! emulator. They read one `MemoryImage` per boundary, taken with the shim's bulk read and
//! retained from the evaluation of `k-1 -> k` through the decision at `k`, plus the ROM as a
//! `Cartridge`. Both arms get the same decoded channels from the same stub readout (the shipping
//! decoder fed synthetic rates, as `rom_macros_mode.rs` does).
//!
//! ## What is compared, every frame
//!
//! - **The deal and the pad:** bound channels, scene, the feed's palette strip, the running macro,
//!   the empty-pad clock, and the rollback's "nearer the objective" signal.
//! - **The decision:** mask, macro events and silence reason from `decide`.
//! - **The evaluation:** reward events, the abandonment `observe` reports, progress, location,
//!   snapshot safety and game over; the adapter's whole exported ledger every 30 frames.
//! - **Every `MacroState` answer**, through `PokeState` with the adapter's ledger: scene, player,
//!   map size, party, battle, text box, start menu, shop, PC, money, bag, NPCs on and off screen,
//!   signs, warps, connections, walkability, counters, pushed/visited tiles around the player,
//!   script and text latches, the YES/NO prompt, the A latch, frontier exhaustion, the decoded
//!   map grid, the step destination, shop stock, move and item refusals, the exit, map and
//!   objective ledgers, the path layer's exits, frontier and targets, the scene's palette set and
//!   every macro's precondition.
//! - **Rollbacks:** twice per scenario the ratchet's game-only rollback is replayed on both arms
//!   (cancel, import, release buttons, clear transients, re-observe on an image read after the
//!   import), and the events and the next frames must agree.
//!
//! And one thing the shadow can check that the live arm cannot: every address it read is inside
//! the image's captured ranges, so the register windows the image leaves out
//! (`legacy-gameboy-v1` section 8, amended 2026-09-29) are never asked for.

use std::time::{Duration, Instant};

use flybrain_core::decoder::PopulationDecoder;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::ordered::NumberMap;
use flybrain_gb::adapter::MemoryReader;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::pokemon_red::macros::cartridge::{Edge, ExitId, MacroState};
use flybrain_gb::pokemon_red::macros::palette::{MacroKind, precondition, scene_set};
use flybrain_gb::pokemon_red::macros::path;
use flybrain_gb::pokemon_red::state::PokeState;
use flybrain_gb::{
    AdapterLedger, Cartridge, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator,
    GameAdapter, ImageReader, MEMORY_IMAGE_LEN, MemoryImage, captured,
};
use flysim::config::Config;
use flysim::frame::LegacyFrame;
use flysim::macros::{MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;

const MS_PER_FRAME: f64 = 1000.0 / 59.7275;
const FRAMES_PER_SECOND: f64 = 59.7275;
const BURST_MS: f64 = 100.0;
const SEED: u32 = 20_260_916;
const HOT: f64 = 16.0;
const REST: f64 = 10.0;
const HOLDS_PER_SLOT: usize = 3;

/// Frames per scenario; `FLY_MEMIMAGE_FRAMES` overrides.
const DEFAULT_FRAMES: u32 = 3_000;

/// The scenarios: the checkpoint's `rom-env.sh` variable, what it is there to cover, and a
/// channel the stub leans on every other burst. A scenario that leans on `macro_menu` also has
/// the harness open the start menu itself on an overworld boundary every 250 frames (the first idle one after) (START held
/// for three frames on the emulator, then twenty idle, both arms re-observing the image after):
/// the palette's own MENU press does not reliably leave the menu up long enough to be seen.
const SCENARIOS: &[(&str, &str, Option<&str>)] = &[
    ("FLY_CENTER_CHECKPOINT", "centre", None),
    ("FLY_MART_CHECKPOINT", "mart", None),
    ("FLY_CATCH_CHECKPOINT", "forest battles", None),
    ("FLY_GYM_CHECKPOINT", "gym", None),
    ("FLY_DOOR_CHECKPOINT", "overworld door", None),
    ("FLY_BUILDING_CHECKPOINT", "building", None),
    ("FLY_MT_MOON_CHECKPOINT", "cave", None),
    ("FLY_ROW62_MART_CHECKPOINT", "mart 2", None),
    ("FLY_PEWTER_CHECKPOINT", "start menu", Some("macro_menu")),
];

fn rates(hot: Option<&str>) -> NumberMap {
    let mut rates = NumberMap::new();
    for channel in flybrain_gb::macro_channels("pokemon-red") {
        rates.set(channel, REST);
    }
    for bucket in 0..8 {
        rates.set(&format!("command_{bucket}"), REST);
    }
    if let Some(channel) = hot {
        rates.set(channel, HOT);
    }
    rates
}

fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

/// The shadow's reader: the image and the cartridge, counting what is asked of it.
struct Recording<'a> {
    inner: ImageReader<'a>,
    seen: &'a mut Vec<u64>,
    uncaptured: &'a mut u64,
}

impl MemoryReader for Recording<'_> {
    fn read8(&mut self, address: u16) -> u8 {
        let index = usize::from(address);
        self.seen[index >> 6] |= 1 << (index & 63);
        if !captured(address) {
            *self.uncaptured += 1;
        }
        self.inner.read8(address)
    }

    fn read_rom(&mut self, bank: u8, address: u16) -> Option<u8> {
        self.inner.read_rom(bank, address)
    }
}

/// Every `MacroState` answer this frame, as text, in a fixed order.
fn sweep(state: &mut dyn MacroState) -> Vec<String> {
    let mut out = Vec::with_capacity(512);
    macro_rules! q {
        ($label:expr, $value:expr) => {
            out.push(format!("{}={:?}", $label, $value))
        };
    }
    let scene = state.scene();
    q!("scene", scene);
    let player = state.player();
    q!("player", player);
    q!("map_size", state.map_size());
    q!("party", state.party());
    q!("battle", state.battle());
    q!("text_box", state.text_box());
    q!("start_menu", state.start_menu());
    q!("shop", state.shop());
    q!("pc", state.pc());
    q!("money", state.money());
    q!("bag", state.bag());
    q!("npcs", state.npcs());
    q!("offscreen_npcs", state.offscreen_npcs());
    q!("signs", state.signs());
    let warps = state.warps();
    q!("warps", warps);
    q!("connections", state.connections());
    q!("scripted", state.scripted());
    q!("text_open", state.text_open());
    q!("yes_no", state.yes_no_prompt());
    q!("a_latched", state.a_latched());
    q!("frontier_exhausted", state.frontier_exhausted());
    // The grid can be a few thousand tiles; its equality is what matters, so compare it whole
    // through its Debug text but keep only a digest of it.
    let grid = state.map_grid().map(|grid| {
        use sha2::Digest;
        let text = format!("{grid:?}");
        format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
    });
    q!("map_grid", grid);
    q!("stepping_onto", state.stepping_onto());
    q!("shop_stock", state.shop_stock());
    q!("objective", state.objective());
    if let Some(player) = player {
        for y in player.y.saturating_sub(6)..=player.y.saturating_add(6) {
            for x in player.x.saturating_sub(6)..=player.x.saturating_add(7) {
                out.push(format!(
                    "tile({x},{y})={:?}/{}/{}/{}",
                    state.walkable(x, y),
                    state.counter_tile(x, y),
                    state.pushed_tile(x, y),
                    state.tile_visited(x, y),
                ));
            }
        }
    }
    for index in 0..u8::try_from(warps.len()).unwrap_or(u8::MAX) {
        q!(format!("exit_visited(warp {index})"), state.exit_visited(ExitId::Warp(index)));
    }
    for edge in [Edge::North, Edge::South, Edge::East, Edge::West] {
        q!(format!("exit_visited({edge:?})"), state.exit_visited(ExitId::Edge(edge)));
    }
    let maps: Vec<u8> = (0..=u8::MAX).filter(|&map| state.map_visited(map)).collect();
    q!("maps_visited", maps);
    let moves: Vec<u8> = (1..=165).filter(|&id| state.move_without_effect(id)).collect();
    q!("moves_without_effect", moves);
    let items: Vec<u8> = (1..=u8::MAX).filter(|&id| state.item_refused(id)).collect();
    q!("items_refused", items);
    q!("exits", path::exits(state));
    q!("frontier", path::frontier(state));
    q!("interactables", path::interactables(state));
    q!("interactable_targets", path::interactable_targets(state));
    q!("person_targets", path::person_targets(state));
    q!("offscreen_person_targets", path::offscreen_person_targets(state));
    q!("scene_set", scene_set(scene, state));
    let preconditions: Vec<(MacroKind, bool)> =
        MacroKind::ALL.iter().map(|&kind| (kind, precondition(kind, state))).collect();
    q!("preconditions", preconditions);
    out
}

/// What one scenario covered, for the report.
#[derive(Default)]
struct Coverage {
    frames: u64,
    swept: u64,
    answers: u64,
    scenes: std::collections::BTreeMap<&'static str, u64>,
    maps: std::collections::BTreeSet<u32>,
    started: u64,
    rewards: u64,
    rollbacks: u64,
    menus_opened: u64,
    distinct_addresses: u64,
    uncaptured: u64,
    read_time: Duration,
    reads: u64,
    digest_time: Duration,
}

/// Mismatches are collected, not panicked on, so a failure reports its first few frames.
struct Diff {
    label: String,
    first: Vec<String>,
    count: u64,
}

impl Diff {
    fn check(&mut self, frame: u64, what: &str, live: String, shadow: String) {
        if live != shadow {
            self.count += 1;
            if self.first.len() < 8 {
                self.first.push(format!(
                    "{} frame {frame} {what}:\n  live   {live}\n  shadow {shadow}",
                    self.label
                ));
            }
        }
    }
}

/// The shim's bulk read, timed.
fn read_image(gb: &Emulator, image: &mut MemoryImage, cov: &mut Coverage) {
    let start = Instant::now();
    gb.read_memory_image_into(image);
    cov.read_time += start.elapsed();
    cov.reads += 1;
}

fn run(
    rom: &[u8],
    label: &str,
    checkpoint: &flysim::store::Checkpoint,
    frames: u32,
    lean: Option<&str>,
) -> (Coverage, Diff) {
    let mut gb = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge");
    gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
    // The executor's ROM asset: refused unless it is the environment's content.
    let cartridge = Cartridge::verified(rom, &gb.rom_sha256()).expect("the environment's ROM");
    let mut live_adapter = PokemonRedReward::new();
    let mut shadow_adapter = PokemonRedReward::new();
    live_adapter.import_state(&checkpoint.runtime.reward).expect("reward ledger");
    shadow_adapter.import_state(&checkpoint.runtime.reward).expect("reward ledger");

    let channels = flybrain_gb::macro_channels("pokemon-red");
    let preset = gameboy_decoder_config_with_macros(&channels);
    let hold_ms = preset.macros.as_ref().expect("a macro group").hold_ms;
    let mut decoder = PopulationDecoder::new(preset).expect("the preset is well formed");
    decoder.calibrate(&rates(None));
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_string();
    config.macros.mode = MacroMode::Macros;
    config.validate().expect("pokemon-red has a palette");
    let mut live: MacroLayer = macro_layer(&config, hold_ms, SEED).expect("a layer");
    let mut shadow: MacroLayer = macro_layer(&config, hold_ms, SEED).expect("a layer");

    let mut cov = Coverage::default();
    let mut diff = Diff { label: label.to_string(), first: Vec::new(), count: 0 };
    let mut seen = vec![0u64; MEMORY_IMAGE_LEN / 64];
    let mut uncaptured = 0u64;

    // O[0]: the boundary's image, retained until the next transition is evaluated.
    let mut image = MemoryImage::zeroed();
    read_image(&gb, &mut image, &mut cov);

    let mut ms = 0.0;
    {
        let a = live.observe(&mut gb, &AdapterLedger(&live_adapter), ms);
        let mut reader = Recording {
            inner: ImageReader::new(&image, &cartridge),
            seen: &mut seen,
            uncaptured: &mut uncaptured,
        };
        let b = shadow.observe(&mut reader, &AdapterLedger(&shadow_adapter), ms);
        diff.check(0, "initial observe", format!("{a:?}"), format!("{b:?}"));
    }

    let mut frame = LegacyFrame::new();
    let mut next_burst = ms;
    let mut burst = 0usize;
    // The ratchet's slot: a state taken early, restored twice.
    let mut slot: Option<Vec<u8>> = None;
    let rollback_at = [frames / 3, 2 * frames / 3];
    let mut menu_due = false;

    for k in 0..u64::from(frames) {
        // --- The deal and the pad, at boundary k.
        let bound = live.bound_channels();
        diff.check(k, "bound", format!("{bound:?}"), format!("{:?}", shadow.bound_channels()));
        diff.check(k, "scene", live.scene_name().into(), shadow.scene_name().into());
        diff.check(
            k,
            "palette",
            format!("{:?}", live.feed_palette()),
            format!("{:?}", shadow.feed_palette()),
        );
        diff.check(k, "running", format!("{:?}", live.running()), format!("{:?}", shadow.running()));
        diff.check(
            k,
            "pad_empty_ms",
            format!("{:?}", live.pad_empty_ms(ms)),
            format!("{:?}", shadow.pad_empty_ms(ms)),
        );
        diff.check(
            k,
            "nearer",
            format!("{:?}", live.nearer_the_objective()),
            format!("{:?}", shadow.nearer_the_objective()),
        );
        *cov.scenes.entry(live.scene_name()).or_default() += 1;
        if let Some(map) = live_adapter.map_id() {
            cov.maps.insert(map);
        }

        // --- Every MacroState answer over O[k], both ways.
        {
            let live_answers = {
                let ledger = AdapterLedger(&live_adapter);
                let mut state = PokeState::with_ledger(&mut gb, &ledger);
                sweep(&mut state)
            };
            let shadow_answers = {
                let ledger = AdapterLedger(&shadow_adapter);
                let mut reader = Recording {
                    inner: ImageReader::new(&image, &cartridge),
                    seen: &mut seen,
                    uncaptured: &mut uncaptured,
                };
                let mut state = PokeState::with_ledger(&mut reader, &ledger);
                sweep(&mut state)
            };
            cov.swept += 1;
            cov.answers += live_answers.len() as u64;
            if live_answers != shadow_answers {
                if live_answers.len() != shadow_answers.len() {
                    diff.check(
                        k,
                        "answer count",
                        live_answers.len().to_string(),
                        shadow_answers.len().to_string(),
                    );
                }
                for (a, b) in live_answers.iter().zip(&shadow_answers) {
                    diff.check(k, "MacroState", a.clone(), b.clone());
                }
            }
        }

        // --- The stub readout, then Phase B on both arms.
        let bursting = ms < next_burst + BURST_MS;
        let rotation = channels[(burst / HOLDS_PER_SLOT) % channels.len()];
        let hot = bursting.then_some(match lean {
            Some(channel) if burst.is_multiple_of(2) => channel,
            _ => rotation,
        });
        if ms >= next_burst + hold_ms {
            next_burst = ms;
            burst += 1;
        }
        let active = decoder.decode_bound(&rates(hot), ms, false, None, Some(&bound));
        let executed = frame.execute(Some(&mut live), &active, 0, ms, &mut gb, &live_adapter);
        let decided = {
            let mut reader = Recording {
                inner: ImageReader::new(&image, &cartridge),
                seen: &mut seen,
                uncaptured: &mut uncaptured,
            };
            shadow.decide(&active, 0, ms, &mut reader, &AdapterLedger(&shadow_adapter))
        };
        diff.check(k, "mask", executed.mask.to_string(), decided.mask.to_string());
        diff.check(k, "events", format!("{:?}", executed.events), format!("{:?}", decided.events));
        diff.check(
            k,
            "silence",
            format!("{:?}", executed.silence),
            format!("{:?}", decided.silence),
        );
        cov.started += executed.events.iter().filter(|event| event.outcome.is_none()).count() as u64;

        // --- The environment runs the live mask; O[k+1] is read once, in bulk.
        ms += MS_PER_FRAME;
        let evaluated = frame
            .stub_advance(Some(&mut live), &mut gb, &mut live_adapter, ms)
            .expect("a frame");
        read_image(&gb, &mut image, &mut cov);
        if k % 64 == 0 {
            let start = Instant::now();
            let _ = image.sha256_hex();
            cov.digest_time += start.elapsed();
        }

        // --- Phase C on the shadow, over the image alone.
        let (rewards, abandoned) = {
            let mut reader = Recording {
                inner: ImageReader::new(&image, &cartridge),
                seen: &mut seen,
                uncaptured: &mut uncaptured,
            };
            let rewards = shadow_adapter.sample(&mut reader, ms);
            let abandoned = shadow.observe(&mut reader, &AdapterLedger(&shadow_adapter), ms);
            (rewards, abandoned)
        };
        cov.rewards += evaluated.rewards.len() as u64;
        diff.check(k, "rewards", format!("{:?}", evaluated.rewards), format!("{rewards:?}"));
        diff.check(k, "abandoned", format!("{:?}", evaluated.abandoned), format!("{abandoned:?}"));
        diff.check(
            k,
            "progress",
            format!("{:?}", evaluated.progress),
            format!("{:?}", shadow_adapter.progress()),
        );
        diff.check(
            k,
            "location",
            format!("{:?}", live_adapter.location()),
            format!("{:?}", shadow_adapter.location()),
        );
        diff.check(
            k,
            "safe/over",
            format!("{}/{}", live_adapter.safe_for_snapshot(), live_adapter.game_over()),
            format!("{}/{}", shadow_adapter.safe_for_snapshot(), shadow_adapter.game_over()),
        );
        if k % 30 == 0 {
            diff.check(
                k,
                "adapter ledger",
                live_adapter.export_state().to_string(),
                shadow_adapter.export_state().to_string(),
            );
        }

        // --- The start-menu scenario: the harness opens the menu, both arms observe the result.
        if lean == Some("macro_menu") && k % 250 == 125 {
            menu_due = true;
        }
        if menu_due
            && live.scene_name() == "overworld"
            && live.running().is_none()
        {
            for (mask, frames) in [(flybrain_gb::buttons::START, 3), (0, 20)] {
                gb.set_buttons(mask);
                for _ in 0..frames {
                    gb.run_frame().expect("a frame");
                }
            }
            let _ = gb.take_audio_u8();
            let a = live.observe(&mut gb, &AdapterLedger(&live_adapter), ms);
            read_image(&gb, &mut image, &mut cov);
            let b = {
                let mut reader = Recording {
                    inner: ImageReader::new(&image, &cartridge),
                    seen: &mut seen,
                    uncaptured: &mut uncaptured,
                };
                shadow.observe(&mut reader, &AdapterLedger(&shadow_adapter), ms)
            };
            diff.check(k, "menu opened", format!("{a:?}"), format!("{b:?}"));
            menu_due = false;
            cov.menus_opened += 1;
        }

        // --- The ratchet's slot and its rollback, on both arms.
        if k == 100 {
            slot = Some(gb.export_state().expect("state"));
        }
        if rollback_at.contains(&(k as u32))
            && let Some(state) = slot.as_ref()
        {
            let a = {
                let mut events = live.cancel(ms);
                gb.import_state(state).expect("the slot");
                gb.set_buttons(0);
                live_adapter.clear_transient();
                events.extend(live.observe(&mut gb, &AdapterLedger(&live_adapter), ms));
                events
            };
            // The environment's RestoreSlot returns "a memory image read after the import".
            read_image(&gb, &mut image, &mut cov);
            let b = {
                let mut events = shadow.cancel(ms);
                shadow_adapter.clear_transient();
                let mut reader = Recording {
                    inner: ImageReader::new(&image, &cartridge),
                    seen: &mut seen,
                    uncaptured: &mut uncaptured,
                };
                events.extend(shadow.observe(&mut reader, &AdapterLedger(&shadow_adapter), ms));
                events
            };
            diff.check(k, "rollback", format!("{a:?}"), format!("{b:?}"));
            cov.rollbacks += 1;
        }
        cov.frames += 1;
    }
    diff.check(
        u64::from(frames),
        "final adapter ledger",
        live_adapter.export_state().to_string(),
        shadow_adapter.export_state().to_string(),
    );
    diff.check(
        u64::from(frames),
        "counts",
        format!("{:?}", live.counts()),
        format!("{:?}", shadow.counts()),
    );
    cov.distinct_addresses = seen.iter().map(|word| u64::from(word.count_ones())).sum();
    cov.uncaptured = uncaptured;
    (cov, diff)
}

#[test]
fn the_macros_over_the_image_are_the_macros_over_the_emulator() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let frames = std::env::var("FLY_MEMIMAGE_FRAMES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_FRAMES);
    let mut all_scenes = std::collections::BTreeSet::new();
    let mut all_maps = std::collections::BTreeSet::new();
    let mut failures = Vec::new();
    let mut ran = 0;
    let (mut read_time, mut reads, mut total_frames, mut total_answers) =
        (Duration::ZERO, 0u64, 0u64, 0u64);
    for &(variable, label, lean) in SCENARIOS {
        let Some(path) = std::env::var_os(variable) else {
            eprintln!("{label}: skipped, {variable} is not set");
            continue;
        };
        let checkpoint = flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope");
        let started = Instant::now();
        let (cov, diff) = run(&rom, label, &checkpoint, frames, lean);
        ran += 1;
        eprintln!(
            "{label} ({variable}): {} frames, {} sweeps / {} answers, {} macros started, {} rewards, \
             {} rollbacks, {} menus opened, {} distinct addresses read ({} uncaptured reads), maps {:x?}, scenes {:?}; \
             bulk read {:.1} us mean; digest {:.1} us mean; {} mismatches; {:.0} s",
            cov.frames,
            cov.swept,
            cov.answers,
            cov.started,
            cov.rewards,
            cov.rollbacks,
            cov.menus_opened,
            cov.distinct_addresses,
            cov.uncaptured,
            cov.maps,
            cov.scenes,
            cov.read_time.as_secs_f64() * 1e6 / cov.reads.max(1) as f64,
            cov.digest_time.as_secs_f64() * 1e6 / cov.frames.div_ceil(64).max(1) as f64,
            diff.count,
            started.elapsed().as_secs_f64(),
        );
        for line in &diff.first {
            eprintln!("{line}");
        }
        if diff.count > 0 {
            failures.push(format!("{label}: {} mismatches", diff.count));
        }
        if cov.uncaptured > 0 {
            failures.push(format!("{label}: {} reads outside the captured ranges", cov.uncaptured));
        }
        all_scenes.extend(cov.scenes.keys().copied());
        all_maps.extend(cov.maps.iter().copied());
        read_time += cov.read_time;
        reads += cov.reads;
        total_frames += cov.frames;
        total_answers += cov.answers;
    }
    if ran == 0 {
        eprintln!("skipped: no scenario checkpoint is set (source rom-env.sh)");
        return;
    }
    let mean = read_time.as_secs_f64() / reads.max(1) as f64;
    eprintln!(
        "total: {ran} scenarios, {total_frames} frames, {total_answers} MacroState answers equal; \
         scenes {all_scenes:?}; bulk read {:.1} us mean per boundary = {:.2}% of a {:.2} ms frame; \
         {:.2} MB/s of image at {FRAMES_PER_SECOND} fps",
        mean * 1e6,
        mean * 1e3 / MS_PER_FRAME * 100.0,
        MS_PER_FRAME,
        MEMORY_IMAGE_LEN as f64 * FRAMES_PER_SECOND / 1e6,
    );
    assert!(failures.is_empty(), "{failures:#?}");
    // The coverage the slice promised, when the full checkpoint set is present.
    if ran == SCENARIOS.len() {
        for scene in ["overworld", "battle", "menu", "dialog", "shop"] {
            assert!(all_scenes.contains(scene), "no {scene} frame in {all_scenes:?}");
        }
        let centres = [0x29u32, 0x3a, 0x40, 0x44];
        assert!(
            centres.iter().any(|map| all_maps.contains(map)),
            "no Pokemon Center in {all_maps:x?}"
        );
    }
}

/// The inspection artifact (`gameboy-memory-inspection-v1`, `legacy-gameboy-v1` section 8): an
/// image encodes as exactly its 65,536 bytes in address order, `application/octet-stream`, with
/// an optional SHA-256 digest in the bus `Digest` encoding, and its `romDigest` is the
/// cartridge's. No ROM needed: a synthetic image and cartridge carry the same shapes.
#[test]
fn an_image_encodes_as_the_memory_inspection_artifact() {
    use fly_session_types::gameboy::{MEMORY_IMAGE_BYTES, MemoryInspection};
    use fly_session_types::workers::AssetRef;
    use fly_session_types::ArtifactRef;
    use flybrain_gb::MEMORY_IMAGE_CONTENT_TYPE;

    assert_eq!(MEMORY_IMAGE_LEN as u64, MEMORY_IMAGE_BYTES);
    let bytes: Vec<u8> = (0..MEMORY_IMAGE_LEN).map(|i| ((i * 31) >> 3) as u8).collect();
    let image = MemoryImage::from_bytes(&bytes).expect("a full image");
    let rom: Vec<u8> = (0..0x8000).map(|i| (i % 251) as u8).collect();
    let cartridge = Cartridge::new(&rom);

    let memory = ArtifactRef {
        store_id: "session".into(),
        artifact_id: "inspection-0".into(),
        generation: 1,
        byte_length: image.as_bytes().len() as u64,
        content_type: MEMORY_IMAGE_CONTENT_TYPE.into(),
        digest: Some(image.sha256_hex()),
    };
    let inspection = MemoryInspection { memory, rom_digest: cartridge.sha256_hex() };
    let typed = inspection.to_typed();
    let back = MemoryInspection::from_typed(&typed).expect("the schema accepts it");
    assert_eq!(back, inspection);
    let asset = AssetRef {
        id: "pokemon-red".into(),
        digest: cartridge.sha256_hex(),
        byte_length: rom.len() as u64,
        format: "gb-rom".into(),
    };
    back.validate_against(&cartridge.sha256_hex(), &asset).expect("one ROM on all three sides");
    // The bytes the artifact carries decode back to the same image.
    assert_eq!(MemoryImage::from_bytes(image.as_bytes()).expect("round trip"), image);

    // A truncated image is refused by both sides of the seam.
    let mut short = inspection.clone();
    short.memory.byte_length = MEMORY_IMAGE_BYTES - 1;
    assert!(MemoryInspection::from_typed(&short.to_typed()).is_err());
    assert!(MemoryImage::from_bytes(&bytes[1..]).is_err());
    // Another ROM is refused.
    let other = Cartridge::new(&rom[1..]);
    assert!(back.validate_against(&other.sha256_hex(), &asset).is_err());
}
