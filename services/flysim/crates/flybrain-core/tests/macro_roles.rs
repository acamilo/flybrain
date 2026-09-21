//! The macro populations of `docs/design/macros.md` section 11, as the dataset carries them.
//!
//! `tools/build_flywire.py` cuts them and this is the check on the committed artifact: that the
//! thirty-one `macro_<type>` roles are exactly the mushroom body output neurons plus the brain
//! motor neurons, dealt round-robin in the contract's own order, and that they reach a network's
//! rate roles without disturbing anything that was there before.
//!
//! Thirty-one since `docs/design/macros.md` sections 13 and 14 (2026-09-17): section 13 added
//! `GO SHOP`, `GO HEAL`, `BUY ANTIDOTE`, `BUY REPEL` and `HEAL`; section 14 replaced `ATTACK`
//! with `MOVE 1`..`MOVE 4`, added `THROW BALL` and renamed `GO STAIRS` to `GO WARP`.
//!
//! 206 neurons dealt thirty-one ways is not twenty-two ways plus nine: the partition is a pure
//! function of the type list, so every population was re-dealt, exactly as it was when section
//! 11's amendment went from six slot channels to twenty-two type channels. The compatibility
//! string is byte-identical across the change (`flysim --print-compatibility`, 648 bytes) because
//! the neuron ids, the edges and the kernel are untouched and rates are restored by role name --
//! a checkpoint written before the change loads, the renamed and the new roles start at zero, and
//! what a run loses is the mushroom body's learned preference for particular macros.
//!
//! Skipped, like every other real-dataset test here, on a branch where `data/fafb-v783` is absent.

mod common;

use std::sync::Arc;

use common::real_dataset_dir;
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::lif::{LifConfig, LifNetwork};
use flybrain_core::plasticity::PlasticityConfig;

/// The thirty-one types, in the order sections 11, 13 and 14 list them, which is the order the
/// populations were cut in. Spelled out rather than derived: this file is the check *on* the
/// artifact, so taking the order from the same place the artifact took it would check nothing.
const MACRO_ROLES: [&str; 31] = [
    "macro_go_objective",
    "macro_go_out",
    "macro_go_warp",
    "macro_go_route",
    "macro_go_item",
    "macro_go_npc",
    "macro_go_frontier",
    "macro_go_shop",
    "macro_go_heal",
    "macro_talk",
    "macro_menu",
    "macro_next",
    "macro_yes",
    "macro_no",
    "macro_close",
    "macro_confirm",
    "macro_back",
    "macro_move_1",
    "macro_move_2",
    "macro_move_3",
    "macro_move_4",
    "macro_switch",
    "macro_item",
    "macro_throw_ball",
    "macro_run",
    "macro_buy_potion",
    "macro_buy_ball",
    "macro_buy_antidote",
    "macro_buy_repel",
    "macro_heal",
    "macro_leave",
];

#[test]
fn the_macro_populations_partition_the_mushroom_body_and_motor_pool() {
    let Some(dir) = real_dataset_dir() else {
        eprintln!("skipped: data/fafb-v783/meta.json is absent in this worktree");
        return;
    };
    let data = load_brain_dataset_from_dir(&dir).expect("the dataset must load");

    // The pool: 96 MBONs and 110 brain motor neurons, disjoint, 206 in all.
    let mbon = data.role("mbon").to_vec();
    let motor = data.role("motor").to_vec();
    assert_eq!(mbon.len(), 96);
    assert_eq!(motor.len(), 110);
    let mut pool: Vec<u32> = mbon.iter().chain(motor.iter()).copied().collect();
    pool.sort_unstable();
    pool.dedup();
    assert_eq!(pool.len(), 206, "the two anatomical roles must not overlap");

    // Round-robin over the sorted pool, in the contract's type order: neuron `pool[i]` belongs to
    // `MACRO_ROLES[i % 31]` and to nothing else.
    let mut expected: Vec<Vec<u32>> = vec![Vec::new(); MACRO_ROLES.len()];
    for (position, neuron) in pool.iter().enumerate() {
        expected[position % MACRO_ROLES.len()].push(*neuron);
    }
    let mut seen: Vec<u32> = Vec::new();
    for (index, role) in MACRO_ROLES.iter().enumerate() {
        let indices = data.role(role);
        assert_eq!(indices, expected[index].as_slice(), "{role}");
        assert!(
            indices.len() == 6 || indices.len() == 7,
            "{role} has {} neurons; a single neuron would be too noisy to read",
            indices.len()
        );
        seen.extend_from_slice(indices);
    }
    // A partition: every pool neuron in exactly one population, and no neuron from outside it.
    seen.sort_unstable();
    assert_eq!(seen, pool, "the populations must partition the pool exactly");
    assert_eq!(seen.len(), 206);

    // Twenty sevens and eleven sixes, which is 206 dealt thirty-one ways.
    let sevens = MACRO_ROLES.iter().filter(|role| data.role(role).len() == 7).count();
    assert_eq!(sevens, 20);
}

#[test]
fn the_macro_roles_are_tracked_rates_and_a_checkpoint_without_them_starts_them_at_zero() {
    let Some(dir) = real_dataset_dir() else {
        eprintln!("skipped: data/fafb-v783/meta.json is absent in this worktree");
        return;
    };
    let data = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset must load"));
    let mut network = LifNetwork::new(
        Arc::clone(&data),
        LifConfig::default(),
        PlasticityConfig::default(),
    )
    .expect("a default network");

    // Tracked by default, exactly as a `command_*` button is, and appended after the roles that
    // were already there: a consumer reading the rates by name sees nothing move.
    let names = network.role_names.clone();
    assert_eq!(
        names.len(),
        14 + MACRO_ROLES.len(),
        "eight buttons, six historical roles, thirty-one macros"
    );
    assert_eq!(
        names[..14].iter().filter(|name| name.starts_with("macro_")).count(),
        0,
        "the historical roles keep their places: {names:?}"
    );
    for role in MACRO_ROLES {
        assert!(names.contains(&role.to_string()), "{role} is not tracked");
    }

    // Run long enough for the populations to fire, then take a checkpoint of it.
    network.step(200);
    let mut state = network.export_state();
    assert!(
        MACRO_ROLES.iter().any(|role| state.rates.get_or_zero(role) > 0.0),
        "the macro populations should be spiking after 200 ms"
    );

    // A checkpoint written before the roles existed carries no entry for them. Rates are restored
    // by name, so the new ones start at zero and nothing else moves -- which is what lets the live
    // checkpoint load across this change (`docs/design/macros.md` section 11).
    let older = flybrain_core::ordered::NumberMap::from_pairs(
        state
            .rates
            .keys()
            .filter(|name| !name.starts_with("macro_"))
            .map(|name| (name.clone(), state.rates.get_or_zero(name)))
            .collect::<Vec<_>>()
            .iter()
            .map(|(name, value)| (name, *value)),
    );
    state.rates = older;
    network.import_state(&state).expect("a checkpoint without the macro roles must load");
    for role in MACRO_ROLES {
        assert_eq!(network.rates.get_or_zero(role), 0.0, "{role} must restore at zero");
    }
    assert!(network.rates.get_or_zero("command_0") >= 0.0);
}
