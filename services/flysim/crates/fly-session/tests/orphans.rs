//! BUS-01 review N2: a worker process never outlives its parent, outside systemd too.
//!
//! The test re-executes its own binary as the "parent" (selected by an environment variable),
//! which spawns one worker through the launcher's `die_with_parent` and one plain control child,
//! then waits. The outer test `kill -9`s the parent and checks the worker is gone within a bound
//! while the control child, which has no death signal, survives (so the check can fail).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ROLE: &str = "FLY_TEST_ORPHAN_PARENT";
const BOUND: Duration = Duration::from_secs(5);

/// True while `pid` is a live process (a zombie awaiting its reaper counts as gone).
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => {
            let state = stat.rsplit(')').next().unwrap_or("").trim().chars().next();
            state != Some('Z') && state != Some('X')
        }
        Err(_) => false,
    }
}

fn wait_gone(pid: u32) -> bool {
    let start = Instant::now();
    while start.elapsed() < BOUND {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_killed_parent_takes_its_workers_with_it() {
    if std::env::var_os(ROLE).is_some() {
        // The parent: spawn a protected worker and an unprotected control, report, then wait.
        let mut worker = Command::new("sleep");
        worker.arg("300");
        fly_session::launcher::die_with_parent(&mut worker);
        let mut worker = worker.spawn().unwrap();
        let mut control = Command::new("sleep").arg("300").spawn().unwrap();
        println!("{} {}", worker.id(), control.id());
        let _ = worker.wait();
        let _ = control.wait();
        return;
    }
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "a_killed_parent_takes_its_workers_with_it", "--nocapture", "--test-threads=1"])
        .env(ROLE, "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(parent.stdout.take().unwrap()).lines();
    let (worker, control) = loop {
        let line = lines.next().expect("the parent reports its children").unwrap();
        let ids: Vec<u32> = line.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        if ids.len() == 2 {
            break (ids[0], ids[1]);
        }
    };
    assert!(alive(worker) && alive(control));
    // SIGKILL: no cleanup runs in the parent, so only the kernel can take the worker down.
    unsafe { libc::kill(parent.id() as i32, libc::SIGKILL) };
    parent.wait().unwrap();
    let worker_gone = wait_gone(worker);
    let control_survived = alive(control);
    unsafe { libc::kill(control as i32, libc::SIGKILL) };
    unsafe { libc::kill(worker as i32, libc::SIGKILL) };
    assert!(worker_gone, "the worker outlived its killed parent");
    assert!(control_survived, "the control child should have survived (the check cannot fail otherwise)");
}
