//! `fly-session`: the lockstep session coordinator and a synthetic composition over Flybus.
//!
//! This crate is the SESSION-01 slice of the session-framework implementation guide: the
//! sequential transaction of [`step-v1`], driven over the [`flybus`] router, with small fake
//! workers standing in for a brain and an emulator.
//!
//! ```text
//! Coordinator ── Agent.Prepare ──> agent workers      (fake model + fixed readout stub)
//!             ── Environment.Advance ──> environment  (a counter arena, no emulator)
//!             ── task.evaluate_transition (once)
//!             ── Agent.Commit ──> agent workers
//!             ── committed snapshot ──> session.<id>.snapshots
//! ```
//!
//! Every arrow is a Flybus RPC to an incarnation-pinned service, with domain request ids and
//! the result caches of `ipc-v1` section 5 in front of every mutation. Nothing here contains a
//! public controller API, an implicit best-effort retry, a real emulator or a real brain.
//!
//! The domain types come from the CONTRACT-01 crate [`fly_session_types`]; [`types`] is a
//! facade over it plus the few session-side additions a coordinator needs.
//!
//! [`step-v1`]: https://example.invalid/step-v1

pub mod agent;
pub mod clock;
pub mod coordinator;
pub mod dedup;
pub mod environment;
pub mod harness;
pub mod phase;
pub mod rpc;
pub mod task;
pub mod worker;

// CONTRACT-01 owns the domain types; `types` is a facade over its crate plus the few
// session-side additions a coordinator needs.
pub mod types;
pub use fly_session_types;

pub use coordinator::{Coordinator, DispatchOrder, Injections, SessionFailure, StepReport};
pub use phase::{Phase, PhaseMachine};
