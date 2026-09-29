//! The `FLYSIM01` checkpoint store and the sugar journal.
//!
//! `FLYSIM01` is the checkpoint format of record for the legacy Game Boy composition until
//! RETIRE-01 (`docs/design/session-framework/legacy-gameboy-v1.md` section 16). Two runtimes write
//! and read it: the legacy loop (`flysim`) and the session runtime (`fly-session`, STATE-02). This
//! crate is the one copy of the code both use, moved here unchanged from `flysim::store` and
//! `flysim::journal`, which re-export it, so a store written by either is the other's store: the
//! same envelope bytes, the same generations and `manifest.json`, the same milestone archives and
//! rotation, and the same restore order.

pub mod journal;
pub mod store;
