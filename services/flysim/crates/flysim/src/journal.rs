//! The sugar journal: every admitted audience input, stamped with the frame it was applied before.
//!
//! The code lives in the `flysim-store` crate, which the session runtime links too (STATE-02).
//! This module re-exports it.

pub use flysim_store::journal::*;
