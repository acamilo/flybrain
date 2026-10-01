//! SHADOW-02 review B1, without a ROM: the real relay and the real `fly-shadow ingest` (over
//! pipes where production has ssh), with the box's shadow stood in for by a thread that keeps its
//! heartbeat and verdict fresh, as a healthy shadow does.
//!
//! After a long outage flysim stops its trace (`"reason":"no-consumer"`) and the live fly runs
//! untraced. When the link heals the box is alive and, counted in trace lines, caught up long
//! before its shadow reaches that stop. The relay must not call that healthy: it sets a sticky
//! `coverageLost`, stops the heartbeat, and clears it only when the verdict's window starts after the stop (a restart alone does not).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use fly_legacy_session::shadow::{
    StopFlag,
    relay::{self, RelayConfig},
    release,
};

const RUN_ID: &str = "1790000000000";
const TRACE: &str = "trace-1790000005000-4242.jsonl";

fn json_of(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn append(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

struct Setup {
    container: PathBuf,
    mirror: PathBuf,
    relay_stop: StopFlag,
    relay: Option<std::thread::JoinHandle<Result<relay::RelayEnd, String>>>,
    shadow_alive: Arc<AtomicBool>,
    window: Arc<AtomicU64>,
    fake_shadow: Option<std::thread::JoinHandle<()>>,
}

impl Setup {
    fn start(root: &Path) -> Setup {
        let container = root.join("container");
        let mirror = root.join("box");
        for d in ["trace", "hot", "durable", "shadow"] {
            std::fs::create_dir_all(container.join(d)).unwrap();
        }
        let fly_shadow = PathBuf::from(env!("CARGO_BIN_EXE_fly-shadow"));
        let script = format!("exec {} ingest --root {}", fly_shadow.display(), mirror.display());
        let release_dir = release::this_release_of(&fly_shadow);
        let mut config = RelayConfig::with_defaults(
            container.join("trace"),
            container.join("hot"),
            container.join("durable"),
            container.join("shadow"),
            vec!["sh".to_owned(), "-c".to_owned(), script],
            RUN_ID.to_owned(),
        );
        config.release = release_dir.display().to_string();
        config.binaries = release::binaries_in(&release_dir).unwrap();
        config.tick = Duration::from_millis(50);
        config.beat_every = Duration::from_millis(300);
        config.alive_within = Duration::from_secs(5);
        config.stall = Duration::from_secs(5);
        config.reconnect_after = Duration::from_secs(1);
        let relay_stop = StopFlag::default();
        let relay = {
            let (config, stop) = (config, relay_stop.clone());
            std::thread::spawn(move || relay::run(config, stop))
        };
        let shadow_alive = Arc::new(AtomicBool::new(true));
        let window = Arc::new(AtomicU64::new(1));
        let fake_shadow = {
            let (mirror, alive, window) = (mirror.clone(), shadow_alive.clone(), window.clone());
            std::thread::spawn(move || {
                // Wait for the run the ingest announces, then behave as a healthy shadow does.
                while fly_legacy_session::shadow::read_run_id(&mirror.join("run-id")).as_deref()
                    != Some(RUN_ID)
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
                while alive.load(Ordering::Relaxed) {
                    let _ = std::fs::write(mirror.join("trace/consumer"), "beat");
                    // 1: a window that has not ended at the stop; 2: a new window (a restart)
                    // that replays the stopped trace and has not reached its stop; 3: one whose
                    // verdict counts from after the stop.
                    let w = window.load(Ordering::Relaxed);
                    let verdict = json!({
                        "runId": RUN_ID, "status": "running", "lagTransitions": 0,
                        "startedAt": format!("window-{}", w.min(2)),
                        "window": {"lastStopTrace": if w >= 3 { json!(TRACE) } else { Value::Null }},
                    });
                    let _ = std::fs::write(
                        mirror.join("out/verdict.json"),
                        serde_json::to_vec(&verdict).unwrap(),
                    );
                    std::thread::sleep(Duration::from_millis(250));
                }
            })
        };
        Setup {
            container,
            mirror,
            relay_stop,
            relay: Some(relay),
            shadow_alive,
            window,
            fake_shadow: Some(fake_shadow),
        }
    }

    fn relay_json(&self) -> Value {
        json_of(&self.container.join("shadow/relay.json"))
    }

    fn heartbeat_age(&self) -> Option<Duration> {
        std::fs::metadata(self.container.join("trace/consumer"))
            .ok()?
            .modified()
            .ok()?
            .elapsed()
            .ok()
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.relay_stop.request();
        self.shadow_alive.store(false, Ordering::Relaxed);
        if let Some(h) = self.relay.take() {
            let _ = h.join();
        }
        if let Some(h) = self.fake_shadow.take() {
            let _ = h.join();
        }
    }
}

/// Reproduces review B1: the box is alive and caught up, yet the live trace ended in a
/// no-consumer stop that its shadow has not reached.
#[test]
fn a_no_consumer_stop_in_the_forwarded_trace_makes_the_relay_unhealthy_until_a_new_window() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Setup::start(tmp.path());
    let trace = s.container.join("trace").join(TRACE);
    append(&trace, b"{\"boot\":true}\n{\"behaviour\":{\"step\":\"1\"}}\n");
    wait_for("a healthy relay that follows the trace", 60, || {
        let r = s.relay_json();
        r["healthy"] == true
    });
    assert!(s.heartbeat_age().is_some(), "a healthy relay beats");

    // The outage: flysim wrote its stop (here in two pieces, as a chunk boundary may cut it).
    append(&trace, b"{\"reason\":\"no-con");
    std::thread::sleep(Duration::from_millis(400));
    append(&trace, b"sumer\",\"truncated\":true}\n");
    std::thread::sleep(Duration::from_secs(3));
    let r = s.relay_json();
    assert_eq!(
        r["healthy"], false,
        "B1: the box is alive and caught up, but the live fly now runs untraced: {r}"
    );
    assert_eq!(r["coverageLost"]["trace"], TRACE, "{r}");
    assert!(r["traceAgeSeconds"].is_u64(), "{r}");

    // The link is fine and the box is alive and caught up, for as long as we care to look: still
    // not healthy, and no heartbeat (flysim would stop any later trace for want of one).
    std::thread::sleep(Duration::from_secs(3));
    let r = s.relay_json();
    assert_eq!(r["remoteAlive"], true, "{r}");
    assert_eq!(r["connected"], true, "{r}");
    assert_eq!(r["healthy"], false, "caught up must not clear a lost coverage: {r}");
    assert!(!r["coverageLost"].is_null(), "sticky: {r}");
    assert!(
        s.heartbeat_age().is_none_or(|age| age >= Duration::from_secs(2)),
        "an unhealthy relay does not beat"
    );
    // Still in the same window after the shadow's own reports: sticky.
    std::thread::sleep(Duration::from_secs(1));
    assert!(!s.relay_json()["coverageLost"].is_null());

    // B2: a restarted shadow (a new window) that has not ended its window at the stop still
    // counts the time before the gap: the loss stays.
    s.window.store(2, Ordering::Relaxed);
    std::thread::sleep(Duration::from_secs(2));
    assert!(!s.relay_json()["coverageLost"].is_null());
    assert_eq!(s.relay_json()["healthy"], false);
    // Its verdict counts from after the stop: closed.
    s.window.store(3, Ordering::Relaxed);
    wait_for("the new window to clear the stop", 60, || {
        let r = s.relay_json();
        r["coverageLost"].is_null() && r["healthy"] == true
    });
    let _ = &s.mirror;
}

/// A stop of another kind (the byte cap) is an allowed skip and does not lose coverage.
#[test]
fn a_byte_cap_stop_is_not_a_lost_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Setup::start(tmp.path());
    let trace = s.container.join("trace").join(TRACE);
    append(&trace, b"{\"boot\":true}\n{\"reason\":\"byte-cap\",\"truncated\":true}\n");
    std::thread::sleep(Duration::from_secs(3));
    let r = s.relay_json();
    assert!(r["coverageLost"].is_null(), "{r}");
    wait_for("a healthy relay", 60, || s.relay_json()["healthy"] == true);
}

/// A relay restart within the run keeps what the first relay saw.
#[test]
fn a_lost_coverage_survives_a_relay_restart_of_the_same_run() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Setup::start(tmp.path());
    let trace = s.container.join("trace").join(TRACE);
    append(
        &trace,
        b"{\"boot\":true}\n{\"reason\":\"no-consumer\",\"truncated\":true}\n",
    );
    wait_for("the relay to see the stop", 60, || !s.relay_json()["coverageLost"].is_null());
    drop(s);
    // The stopped relay wrote relay.json last; a new relay of the same run (the unit's restart)
    // starts from it, reconnects to the same mirror, and is alive and caught up, but not healthy.
    let s = Setup::start(tmp.path());
    wait_for("the box reported alive", 60, || s.relay_json()["remoteAlive"] == true);
    std::thread::sleep(Duration::from_secs(2));
    let r = s.relay_json();
    assert_eq!(r["coverageLost"]["trace"], TRACE, "{r}");
    assert_eq!(r["remoteAlive"], true, "{r}");
    assert_eq!(r["healthy"], false, "{r}");
}
