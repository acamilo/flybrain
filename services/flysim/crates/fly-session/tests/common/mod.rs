//! Shared test fixture: the synthetic session on a temporary store, over either transport.

#![allow(dead_code)]

use std::time::Duration;

use fly_session::harness::{ExecutionMode, HarnessConfig, SessionHarness, Via};
use fly_session::types::*;

pub const WAIT: Duration = Duration::from_secs(20);

/// Generates one test per transport from an `async fn name(via: Via)`.
#[macro_export]
macro_rules! both_transports {
    ($($name:ident),* $(,)?) => {
        mod in_memory {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name($crate::common::via_memory()).await
                }
            )*
        }
        mod unix_socket {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name($crate::common::via_unix()).await
                }
            )*
        }
    };
}

pub fn via_memory() -> Via {
    Via::Memory
}

pub fn via_unix() -> Via {
    Via::Unix
}

/// A started session plus the temporary directory its store lives in.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub harness: SessionHarness,
}

impl Fixture {
    pub async fn shutdown(self) {
        let Fixture { dir, harness } = self;
        harness.shutdown().await;
        drop(dir);
    }
}

pub async fn fixture(via: Via, config: HarnessConfig) -> Fixture {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let harness = SessionHarness::start(via, dir.path(), config)
        .await
        .expect("the synthetic session starts");
    Fixture { dir, harness }
}

/// The default two-agent composition: 60 Hz world, 1 ms model tick.
pub async fn default_fixture(via: Via) -> Fixture {
    fixture(via, HarnessConfig::default()).await
}

pub fn fly_a() -> Id {
    id("fly-a")
}

pub fn fly_b() -> Id {
    id("fly-b")
}

/// The index of an audit entry, or a panic naming what was missing.
pub fn at(audit: &[String], what: &str) -> usize {
    audit
        .iter()
        .position(|entry| entry == what)
        .unwrap_or_else(|| panic!("the audit has no {what:?}: {audit:?}"))
}

pub fn count(audit: &[String], what: &str) -> usize {
    audit.iter().filter(|entry| *entry == what).count()
}

/// Fails the test rather than hanging, so a missed reply is a failure and not a stuck job.
pub async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(WAIT, f).await {
        Ok(v) => v,
        Err(_) => panic!("{what}: timed out"),
    }
}

/// Generates one test per execution mode from an `async fn name(mode: ExecutionMode)`.
///
/// The separate-process mode is the SESSION-02 subject; the other two are the variants it is
/// compared against, and a row that holds in one must hold in all three.
#[macro_export]
macro_rules! all_modes {
    ($($name:ident),* $(,)?) => {
        mod in_process {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name($crate::common::mode_in_process()).await
                }
            )*
        }
        mod dedicated_thread {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name($crate::common::mode_thread()).await
                }
            )*
        }
        mod separate_process {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name($crate::common::mode_process()).await
                }
            )*
        }
    };
}

pub fn mode_in_process() -> ExecutionMode {
    ExecutionMode::InProcess
}

pub fn mode_thread() -> ExecutionMode {
    ExecutionMode::Thread
}

pub fn mode_process() -> ExecutionMode {
    ExecutionMode::Process
}

/// A fixture in one execution mode. The transport is the mode's own: a separate process
/// reaches the router only over a socket.
pub async fn mode_fixture(mode: ExecutionMode, config: HarnessConfig) -> Fixture {
    fixture(Via::Unix, HarnessConfig { mode, ..config }).await
}
