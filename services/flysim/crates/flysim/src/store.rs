//! Checkpoint persistence: the `FLYSIM01` envelope, the atomic commit, the generations and the
//! milestone archives.
//!
//! The code lives in the `flysim-store` crate, which the session runtime links too (STATE-02), so
//! both write byte-identical checkpoints into the same store. This module re-exports it, and every
//! `flysim::store` path keeps working.

pub use flysim_store::store::*;
