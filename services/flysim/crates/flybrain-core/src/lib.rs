//! The flybrain neural core, ported from `packages/brain/src` and bit-exact with it under the
//! default configuration.
//!
//! The TypeScript library is the oracle. `services/flysim/golden/` holds state dumps produced by
//! `packages/brain/tools/golden.ts`, and `tests/` replays those scenarios and compares exactly;
//! see this crate's README for what "exactly" is verified to mean and what the parallel sweep is
//! and is not allowed to change.
//!
//! ```no_run
//! use std::sync::Arc;
//! use flybrain_core::agent::{AgentConfig, NeuralAgent, TickOptions};
//! use flybrain_core::dataset::load_brain_dataset_from_dir;
//! use flybrain_core::decoder::gameboy::{gameboy_decoder_config, to_button_mask};
//! use flybrain_core::lif::SweepPlan;
//!
//! let data = Arc::new(load_brain_dataset_from_dir("data/fafb-v783")?);
//! let mut agent = NeuralAgent::new(data, AgentConfig::with_decoder(gameboy_decoder_config()))?;
//! agent.set_sweep_plan(SweepPlan::with_threads(6)?);
//! let frame = vec![0u8; 160 * 144 * 4];
//! agent.warmup(Some(&frame))?;
//! let result = agent.tick(&frame, &TickOptions::default())?;
//! let buttons = to_button_mask(&result.active);
//! # Ok::<(), flybrain_core::Error>(())
//! ```

pub mod agent;
pub mod bitset;
pub mod dataset;
pub mod decoder;
pub mod envelope;
pub mod error;
pub mod jsmath;
pub mod json;
pub mod lif;
pub mod ordered;
pub mod plasticity;
pub mod pool;
pub mod retina;
pub mod rng;
pub mod version;

pub use error::{Error, Result};
