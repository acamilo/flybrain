//! The live fly on the session framework (TASK-01).
//!
//! - [`task`]: `pokered-macros-v1`, the Pokémon Red task and its action executor as one object,
//!   running the legacy macro engine and reward adapter unchanged over the boundary memory image.
//! - [`composition`]: the legacy agent, the legacy environment and the task under one coordinator.
//! - [`import`]: a FLYSIM01 checkpoint as the session's start.
//! - [`admission`]: sugar by the legacy rules; the operator reward pulse refused.
//! - [`trace`]: the session's `FLY_TRACE`, in the legacy loop's format, and the comparison.
//! - [`driver`]: a stub readout for parity runs that must earn rewards.
//!
//! `docs/design/session-framework/legacy-gameboy-v1.md` is the contract, `implementation.md`
//! TASK-01 the record.

pub mod admission;
pub mod composition;
pub mod driver;
pub mod import;
pub mod task;
pub mod trace;
