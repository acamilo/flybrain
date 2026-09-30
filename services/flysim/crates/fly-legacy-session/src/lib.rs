//! The live fly on the session framework (TASK-01).
//!
//! - [`task`]: `pokered-macros-v1`, the Pokémon Red task and its action executor as one object,
//!   running the legacy macro engine and reward adapter unchanged over the boundary memory image.
//! - [`composition`]: the legacy agent, the legacy environment and the task under one coordinator.
//! - [`import`]: a FLYSIM01 checkpoint as the session's start.
//! - [`admission`]: sugar by the legacy rules; the operator reward pulse refused.
//! - [`trace`]: the session's `FLY_TRACE`, in the legacy loop's format, and the comparison.
//! - [`driver`]: a stub readout for parity runs that must earn rewards.
//! - [`service`]: the composition as the live service behind flysim's listeners (SERVE-01),
//!   the `flysim-session` binary.
//! - [`shadow`]: SHADOW-01, the session runtime following the live fly's trace, the comparator and
//!   the verdict CUT-01 reads.
//!
//! `docs/design/session-framework/legacy-gameboy-v1.md` is the contract, `implementation.md`
//! TASK-01 the record.

pub mod admission;
pub mod composition;
pub mod driver;
pub mod import;
pub mod service;
pub mod shadow;
pub mod task;
pub mod trace;
