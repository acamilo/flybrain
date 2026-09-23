//! Rebuild a live session's macro ledgers from the environment, because a restore starts them
//! empty (`docs/design/macros.md` section 12: the ledgers are session state and never reach the
//! checkpoint). Shared by `scene_probe` and `trap_hunt`; `infra/docs/macros-traps.md` row 57 is
//! the trap that needed it: the pad that stalled was dealt by ledgers no checkpoint carries.
//!
//! Every variable is `<PREFIX>_<NAME>`, so the two examples keep their own namespaces:
//!
//! | name | meaning |
//! | --- | --- |
//! | `PUSHED` | `"x,y;x,y"`: tiles of the loaded map walled the way a scripted push-back walls them |
//! | `EXHAUSTED` | `1`: the loaded map's frontier marked unreachable (section 12.14) |
//! | `TALKED` | `1`: every person and sign on the loaded map written into the talked ledger |
//! | `BLOCKED` | `"warp2;east"`: those exits rested for the blocked window |
//! | `STOOD` | `1`: the adapter's covered ground; `all`: every tile of the map (no new ground at all) |
//!
//! The tile underfoot is always recorded as stood on, or the first observe would read it as new
//! ground and clear the frontier mark being rebuilt. Nothing is seeded when no variable is set.

use flybrain_gb::pokemon_red::macros::PokemonPalette;
use flybrain_gb::pokemon_red::macros::cartridge::{Edge, ExitId, MacroState, TargetKey};
use flybrain_gb::pokemon_red::macros::{Tile, path};
use flybrain_gb::pokemon_red::{PokemonRedReward, state};
use flybrain_gb::{AdapterLedger, Emulator, MacroPalette};

/// Seed `macros` from `<prefix>_*` at brain millisecond `ms`, and say what was seeded.
pub fn seed(
    macros: &mut PokemonPalette,
    gb: &mut Emulator,
    adapter: &PokemonRedReward,
    ms: f64,
    prefix: &str,
) -> Option<String> {
    let var = |name: &str| std::env::var(format!("{prefix}_{name}")).ok();
    let names = ["PUSHED", "EXHAUSTED", "TALKED", "BLOCKED", "STOOD"];
    if names.iter().all(|name| var(name).is_none()) {
        return None;
    }
    let player = state::player(gb)?;
    let map = player.map;
    let stood_mode = var("STOOD").unwrap_or_default();
    macros.clock(ms);
    let (things, covered): (Vec<_>, Vec<Tile>) =
        macros.inspect(gb, &AdapterLedger(adapter), |state: &mut dyn MacroState| {
            let mut all = path::person_targets(state);
            all.extend(path::interactable_targets(state));
            let mut covered = Vec::new();
            if let Some(size) = state.map_size() {
                for y in 0..size.height {
                    for x in 0..size.width {
                        if stood_mode == "all" || state.tile_visited(x, y) {
                            covered.push(Tile::new(x, y));
                        }
                    }
                }
            }
            (all, covered)
        });
    let (talked, targets, stood, pushed, frontiers) = macros.ledgers_mut();
    targets.clock(ms);
    stood.record(map, Tile::new(player.x, player.y));
    if stood_mode == "1" || stood_mode == "all" {
        for tile in &covered {
            stood.record(map, *tile);
        }
    }
    for pair in var("PUSHED").unwrap_or_default().split(';').filter(|pair| !pair.is_empty()) {
        let mut xy = pair.split(',').map(|n| n.trim().parse::<u8>().expect("PUSHED is x,y;x,y"));
        let (x, y) = (xy.next().expect("an x"), xy.next().expect("a y"));
        pushed.record(map, Tile::new(x, y));
    }
    if var("EXHAUSTED").as_deref() == Some("1") {
        frontiers.record(map);
    }
    if var("TALKED").as_deref() == Some("1") {
        for (_, target) in &things {
            talked.record(map, *target);
        }
    }
    for key in var("BLOCKED").unwrap_or_default().split(';').filter(|key| !key.is_empty()) {
        let id = match key {
            "north" => ExitId::Edge(Edge::North),
            "south" => ExitId::Edge(Edge::South),
            "east" => ExitId::Edge(Edge::East),
            "west" => ExitId::Edge(Edge::West),
            warp => ExitId::Warp(warp.trim_start_matches("warp").parse().expect("BLOCKED warpN")),
        };
        targets.record_blocked(map, TargetKey::Exit(id));
    }
    Some(format!(
        "map {map:#04x}: pushed {pushed:?}, frontier marks {frontiers:?}, {} things talked to, \
         {} tiles stood on",
        talked.len(),
        stood.len()
    ))
}
