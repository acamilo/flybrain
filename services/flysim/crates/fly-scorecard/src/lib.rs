//! The behavioural scorecard (B2): a repeatable multi-seed regression harness for good play.
//!
//! A fixed set of checkpoints, K seeds each, T brain minutes each, on the real connectome with the
//! macro layer, on the session runtime (default) or the legacy loop. Every run reduces to one
//! [`tally::RunReport`]; a suite is the set of runs ([`suite::SuiteReport`]); two suites compare
//! with a verdict per metric ([`compare::compare`]), by a test over the paired runs rather than
//! by one run.
//!
//! The harness measures and presses nothing. See `docs/scorecard.md`.

pub mod compare;
pub mod obs;
pub mod runner;
pub mod suite;
pub mod tally;
pub mod watchdog;
