//! Unit tests ported from the loop half of `packages/brain/tests/agent.test.ts`.
//!
//! The frame clock, the warm-up guards, `reset_transients` and — the case that needs the most
//! care — the import rollback: the network and the readout validate themselves separately, so a
//! checkpoint whose network half is valid and whose readout half is not would leave a frame-100
//! network running under a frame-200 readout.

mod common;

use std::sync::Arc;

use common::{frame_pool, synthetic_dataset, toy_dataset};
use flybrain_core::agent::{
    AgentConfig, AgentState, FrameSize, NeuralAgent, RewardEvent, TickOptions, GAMEBOY_MS_PER_FRAME,
};
use flybrain_core::decoder::gameboy::gameboy_decoder_config;

fn gameboy_config(width: u32, height: u32) -> AgentConfig {
    let mut config = AgentConfig::with_decoder(gameboy_decoder_config());
    config.frame = Some(FrameSize { width, height });
    config
}

fn toy_agent(width: u32, height: u32, warmup_ms: u64) -> NeuralAgent {
    let mut config = gameboy_config(width, height);
    config.warmup_ms = warmup_ms;
    NeuralAgent::new(Arc::new(toy_dataset()), config).expect("an agent")
}

fn synthetic_agent() -> (NeuralAgent, Vec<Vec<u8>>) {
    let (data, _) = synthetic_dataset();
    let agent = NeuralAgent::new(data, gameboy_config(160, 144)).expect("an agent");
    (agent, frame_pool(8, 909, 160, 144))
}

fn rewards_for(frame: usize) -> Vec<RewardEvent> {
    if frame.is_multiple_of(137) {
        vec![
            RewardEvent::with_stimulation(-0.4, 40.0),
            RewardEvent::new(0.25),
        ]
    } else if frame.is_multiple_of(50) {
        vec![RewardEvent::new(0.6)]
    } else {
        Vec::new()
    }
}

#[test]
fn one_game_boy_frame_is_70224_dot_clocks_and_the_loop_never_drifts() {
    assert_eq!(GAMEBOY_MS_PER_FRAME, 1000.0 / (4_194_304.0 / 70_224.0));
    let mut agent = toy_agent(8, 4, 10);
    assert_eq!(agent.ms_per_frame, GAMEBOY_MS_PER_FRAME);
    assert_eq!(agent.warmup_ms, 10);
    assert!(!agent.ready());

    let blank = vec![0u8; 8 * 4 * 4];
    agent.warmup(None).expect("warmup");
    assert!(agent.ready());

    let mut steps = 0u64;
    for _ in 0..1000 {
        steps += agent
            .tick(&blank, &TickOptions::default())
            .expect("tick")
            .steps;
    }
    // 1000 frames at 59.7275 fps is 16742.7 ms: the fractional remainder must be carried, not lost.
    assert_eq!(steps, (1000.0 * GAMEBOY_MS_PER_FRAME).floor() as u64);
    assert_eq!(agent.network.ms, 10.0 + steps as f64);
    let remainder = agent.export_state().remainder;
    assert!((0.0..1.0).contains(&remainder));
}

#[test]
fn warmup_calibrates_once_and_a_tick_before_warmup_is_refused() {
    let mut agent = toy_agent(4, 4, 50);
    let blank = vec![0u8; 4 * 4 * 4];
    assert_eq!(
        agent
            .tick(&blank, &TickOptions::default())
            .unwrap_err()
            .message(),
        "Warm up the agent before ticking it"
    );
    agent.warmup(None).expect("warmup");
    assert_eq!(
        agent.warmup(None).unwrap_err().message(),
        "Agent is already warmed up"
    );

    // A wrong-sized frame is refused before the network advances, not half way through the tick.
    let before = agent.export_state();
    assert!(agent
        .tick(&[0u8; 3], &TickOptions::default())
        .unwrap_err()
        .message()
        .contains("RGBA bytes"));
    assert!(agent
        .reset_transients(&[0u8; 3])
        .unwrap_err()
        .message()
        .contains("RGBA bytes"));
    assert_eq!(agent.export_state(), before);

    // Calibration happens on settled rates with plasticity re-enabled. Roles the dataset does not
    // declare calibrate to zero, which is what makes their score exactly 1 and keeps them silent.
    assert!(agent.plasticity().enabled);
    let baseline = agent.export_state().decoder.baseline;
    assert_eq!(
        baseline.get("command_0"),
        agent.network.rates.get("command_0")
    );
    for index in 1..8 {
        assert_eq!(baseline.get(&format!("command_{index}")), Some(0.0));
    }
    assert!(agent.network.rates.get("command_0").unwrap() > 0.0);
}

#[test]
fn the_frame_size_defaults_to_the_kernel_retina_and_is_validated() {
    let agent = NeuralAgent::new(
        Arc::new(toy_dataset()),
        AgentConfig::with_decoder(gameboy_decoder_config()),
    )
    .expect("an agent");
    assert_eq!(
        agent.frame,
        FrameSize {
            width: 160,
            height: 144
        }
    );

    let mut zero = gameboy_config(0, 4);
    zero.frame = Some(FrameSize {
        width: 0,
        height: 4,
    });
    assert!(NeuralAgent::new(Arc::new(toy_dataset()), zero)
        .unwrap_err()
        .message()
        .contains("frame size"));

    let mut bad_pace = AgentConfig::with_decoder(gameboy_decoder_config());
    bad_pace.ms_per_frame = 0.0;
    assert!(NeuralAgent::new(Arc::new(toy_dataset()), bad_pace)
        .unwrap_err()
        .message()
        .contains("msPerFrame"));
}

#[test]
fn reset_transients_drops_holds_and_traces_but_keeps_the_rng_clock_and_gains() {
    let (data, _) = synthetic_dataset();
    let mut agent = NeuralAgent::new(data, gameboy_config(160, 144)).expect("an agent");
    let frames = frame_pool(8, 77, 160, 144);
    agent.warmup(Some(&frames[0])).expect("warmup");
    for frame in 1..=60usize {
        let rewards = rewards_for(frame);
        agent
            .tick(
                &frames[frame % frames.len()],
                &TickOptions {
                    rewards: &rewards,
                    boot: (frame / 120).is_multiple_of(2),
                    learn: true,
                },
            )
            .expect("tick");
    }

    let before = agent.export_state();
    assert!(
        before.network.plasticity.traces.iter().any(|t| *t != 0.0),
        "no eligibility to clear"
    );
    assert!(
        before.decoder.held_until.values().any(|v| v > 0.0),
        "no hold to clear"
    );

    agent.reset_transients(&frames[3]).expect("reset");
    let after = agent.export_state();

    // Kept: everything the rollback did not invalidate.
    assert_eq!(after.network.ms, before.network.ms);
    assert_eq!(after.network.rng, before.network.rng);
    assert_eq!(
        after.network.population_rate,
        before.network.population_rate
    );
    assert_eq!(after.network.rates, before.network.rates);
    assert_eq!(after.network.membrane, before.network.membrane);
    assert_eq!(
        after.network.plasticity.gains,
        before.network.plasticity.gains
    );
    assert_eq!(
        after.network.plasticity.updates,
        before.network.plasticity.updates
    );
    assert_eq!(after.decoder.baseline, before.decoder.baseline);
    assert_eq!(after.remainder, before.remainder);

    // Dropped: the timeline that no longer exists.
    assert!(after.network.plasticity.traces.iter().all(|t| *t == 0.0));
    assert!(after
        .network
        .plasticity
        .touched
        .iter()
        .all(|t| *t == before.network.ms));
    assert_eq!(after.network.plasticity.signal, 0.0);
    assert!(after.decoder.held_until.values().all(|v| v == 0.0));
    assert!(after
        .decoder
        .next_allowed
        .values()
        .all(|v| v == before.network.ms + 480.0));
    assert_eq!(after.decoder.current, None);
    assert!(after.decoder.fatigue.values().all(|v| v == 0.0));
    assert_eq!(after.decoder.next_decision, before.network.ms);

    // The replacement image became the visual drive.
    assert_ne!(after.network.visual_drive, before.network.visual_drive);
}

#[test]
fn an_exported_agent_resumes_bit_exactly_in_a_fresh_agent() {
    let (mut source, frames) = synthetic_agent();
    source.warmup(Some(&frames[0])).expect("warmup");
    for frame in 1..=200usize {
        let rewards = rewards_for(frame);
        source
            .tick(
                &frames[frame % frames.len()],
                &TickOptions {
                    rewards: &rewards,
                    boot: (frame / 120).is_multiple_of(2),
                    learn: !frame.is_multiple_of(90),
                },
            )
            .expect("tick");
    }

    let (data, _) = synthetic_dataset();
    let mut restored = NeuralAgent::new(data, gameboy_config(160, 144)).expect("an agent");
    assert!(!restored.ready());
    restored
        .import_state(&source.export_state())
        .expect("import");
    assert!(restored.ready());
    assert_eq!(restored.export_state(), source.export_state());
    assert_eq!(restored.compatibility(), source.compatibility());

    for frame in 201..=320usize {
        let rewards = rewards_for(frame);
        let options = TickOptions {
            rewards: &rewards,
            boot: (frame / 120).is_multiple_of(2),
            learn: !frame.is_multiple_of(90),
        };
        let image = &frames[frame % frames.len()];
        assert_eq!(
            restored.tick(image, &options).expect("tick"),
            source.tick(image, &options).expect("tick"),
            "diverged at frame {frame}"
        );
    }
    assert_eq!(restored.export_state(), source.export_state());
    assert_eq!(restored.snapshot(), source.snapshot());
}

#[test]
fn snapshot_reports_the_network_without_exposing_its_arrays() {
    let (mut agent, frames) = synthetic_agent();
    agent.warmup(Some(&frames[0])).expect("warmup");
    let rewards = [RewardEvent::new(1.0)];
    agent
        .tick(
            &frames[1],
            &TickOptions {
                rewards: &rewards,
                boot: true,
                learn: true,
            },
        )
        .expect("tick");

    let snapshot = agent.snapshot();
    assert_eq!(snapshot.ms, agent.network.ms);
    assert_eq!(snapshot.population_rate, agent.network.population_rate);
    assert_eq!(snapshot.rates, agent.network.rates);
    assert_eq!(
        snapshot.spike_times.len(),
        agent.network.last_spike_ms.len()
    );
    // Only viewer timestamps narrow to f32.
    let narrowed: Vec<f32> = agent
        .network
        .last_spike_ms
        .iter()
        .map(|value| *value as f32)
        .collect();
    assert_eq!(snapshot.spike_times, narrowed);
    assert_eq!(snapshot.learning.version, "fly-kc-mbon-rstdp-v2");
    assert!(snapshot.learning.synapses > 0);
}

#[test]
fn a_rejected_checkpoint_leaves_the_agent_exactly_as_it_was() {
    let (data, _) = synthetic_dataset();
    let mut agent =
        NeuralAgent::new(Arc::clone(&data), gameboy_config(160, 144)).expect("an agent");
    let frames = frame_pool(8, 1234, 160, 144);
    agent.warmup(Some(&frames[0])).expect("warmup");
    let run = |agent: &mut NeuralAgent, from: usize, to: usize| {
        for frame in from..=to {
            let rewards = rewards_for(frame);
            agent
                .tick(
                    &frames[frame % frames.len()],
                    &TickOptions {
                        rewards: &rewards,
                        boot: true,
                        learn: true,
                    },
                )
                .expect("tick");
        }
    };
    run(&mut agent, 1, 100);
    let early = agent.export_state();
    run(&mut agent, 101, 200);
    let current = agent.export_state();
    assert_ne!(early, current, "the two checkpoints must differ");

    let mut reject = |state: AgentState, needle: &str| {
        let message = agent
            .import_state(&state)
            .unwrap_err()
            .message()
            .to_string();
        assert!(
            message.contains(needle),
            "expected {needle:?}, got {message:?}"
        );
        assert_eq!(
            agent.export_state(),
            current,
            "a failed import must not change the agent"
        );
    };

    // Version and warm-up flag. (`warmedUp` is a bool by type here, so only the version applies.)
    reject(
        AgentState {
            version: 2,
            ..early.clone()
        },
        "checkpoint version",
    );

    // A remainder outside [0, 1) would desynchronize the network from the environment clock.
    for remainder in [1.25, -0.5, f64::NAN] {
        reject(
            AgentState {
                remainder,
                ..early.clone()
            },
            "frame remainder",
        );
    }

    // Corrupt plasticity: rejected by the network before anything is written.
    let mut bad_gains = early.clone();
    bad_gains.network.plasticity.gains[0] = 5.0;
    reject(bad_gains, "plasticity values");
    let mut bad_topology = early.clone();
    bad_topology.network.plasticity.topology += 1;
    reject(bad_topology, "plasticity topology");

    // Corrupt readout only: the network half imports cleanly first, so this is the case that
    // needs the rollback. Without it the agent would keep a frame-100 network under a frame-200
    // readout.
    let mut bad_decoder = early.clone();
    bad_decoder.decoder.held_until.set("up", f64::INFINITY);
    reject(bad_decoder, "decoder checkpoint");
    let mut bad_fatigue = early.clone();
    bad_fatigue.decoder.fatigue.set("left", 4.0);
    reject(bad_fatigue, "decoder fatigue");

    // The valid state it rolled back from still loads, and the rollback left it loadable.
    agent.import_state(&early).expect("import");
    assert_eq!(agent.export_state(), early);
}

#[test]
fn a_checkpoint_from_a_different_connectome_is_refused_by_dimension() {
    let (data, _) = synthetic_dataset();
    let mut small = toy_agent(160, 144, 10);
    small.warmup(None).expect("warmup");
    let mut large = NeuralAgent::new(data, gameboy_config(160, 144)).expect("an agent");
    large.warmup(None).expect("warmup");
    let before = large.export_state();
    assert!(large
        .import_state(&small.export_state())
        .unwrap_err()
        .message()
        .contains("do not match the loaded dataset"));
    assert_eq!(large.export_state(), before);
}
