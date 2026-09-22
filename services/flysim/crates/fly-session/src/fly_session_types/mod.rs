//! `fly-session-types` as a local module.
//!
//! CONTRACT-01 owns these definitions -- the scalar types, the method payloads, their
//! validation, the canonical digests and the trace format -- as the crate
//! `services/flysim/crates/fly-session-types`. Until that crate exists this module holds the
//! minimum this slice needs, under the same names and field shapes, so the swap is a change of
//! `use` lines in `lib.rs` and nothing else.

pub mod canonical;
pub mod methods;
pub mod scalars;
pub mod trace;

pub use canonical::{canonical_digest, canonical_json};
pub use methods::*;
pub use scalars::*;
pub use trace::*;

/// The identity of the contract revision these types implement.
///
/// A worker reports it in `Worker.Hello`; a coordinator refuses a worker whose value differs,
/// which is what stops a session from mixing two payload revisions.
pub const CONTRACT: &str = "fly-session-types/ipc-v1+step-v1+workers-v1 draft-2 2026-09-18";

/// The SHA-256 of [`CONTRACT`].
pub fn contract_digest() -> Digest {
    Digest::of(CONTRACT.as_bytes())
}
