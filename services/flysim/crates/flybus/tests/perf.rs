//! bus-v1 section 11 item 7 and implementation-guide BUS-03, as a measurement rather than a
//! gate: 640x480 RGBA frames at 60 Hz over a Unix socket to three latest-mode consumers (one
//! delayed 40 ms per frame), with 1, 2 and 4 agent services called every frame.
//!
//! The router runs on its own Tokio runtime whose threads carry a distinct name, so its CPU
//! (routing plus the seal copies on its blocking pool) is measured apart from the clients'.
//! Producer copy cost and consumer readback cost are measured separately from routing. Nothing
//! here is a capacity claim: one host, one process, synthetic payloads.
//!
//! ```text
//! cargo test --release -p flybus --test perf -- --ignored --nocapture
//! ```

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::obj;
use flybus::{
    Client, ClientConfig, Policy, Retained, Router, RouterConfig, ServiceConfig,
    SubscriptionConfig,
};
use serde_json::json;

const W: usize = 640;
const H: usize = 480;
const HZ: u64 = 60;
const SECONDS: u64 = 2;
const ROUTER_THREAD: &str = "flybus-router";
const ROUTER_WORKERS: usize = 2;
const CLIENT_WORKERS: usize = 4;

/// utime+stime of this process, in seconds (fields 14 and 15 of /proc/self/stat, 100 Hz).
fn proc_cpu_seconds() -> f64 {
    thread_cpu_seconds(None)
}

/// utime+stime of the threads whose name matches, in seconds; all of them when `name` is
/// `None`. A thread that exits between two samples takes its time with it, so this is a floor
/// for pools that retire idle threads.
fn thread_cpu_seconds(name: Option<&str>) -> f64 {
    let mut total = 0.0;
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return 0.0;
    };
    for entry in dir.flatten() {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((head, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        if let Some(want) = name {
            let comm = head.split_once('(').map_or("", |(_, c)| c);
            if comm != want {
                continue;
            }
        }
        let f: Vec<&str> = rest.split_whitespace().collect();
        let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        total += (ticks(11) + ticks(12)) / 100.0;
    }
    total
}

fn proc_status(key: &str) -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find(|l| l.starts_with(key))
        .map_or("?".into(), |l| l[key.len()..].trim().to_owned())
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1000.0)
}

fn percentiles(label: &str, v: &mut [Duration]) -> String {
    v.sort();
    format!(
        "{label} ms p50/p95/p99: {}/{}/{}",
        ms(pct(v, 0.5)),
        ms(pct(v, 0.95)),
        ms(pct(v, 0.99))
    )
}

/// The router on its own runtime, reached over a Unix socket.
struct Host {
    router: Router,
    socket: PathBuf,
    store_root: PathBuf,
    rt: Option<tokio::runtime::Runtime>,
    _dir: tempfile::TempDir,
}

impl Host {
    fn start() -> Host {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        let socket = dir.path().join("bus.sock");
        let mut config = RouterConfig::new(&store_root);
        config.policy = Policy::open();
        let router = Router::new(config).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(ROUTER_WORKERS)
            .thread_name(ROUTER_THREAD)
            .enable_all()
            .build()
            .unwrap();
        // The listener, and so every connection task, belongs to the router's runtime.
        let (ready, started) = std::sync::mpsc::channel();
        let (r, s) = (router.clone(), socket.clone());
        rt.spawn(async move {
            let _listener = r.listen_unix(&s).await.expect("the router listens");
            ready.send(()).expect("start() is waiting");
            std::future::pending::<()>().await
        });
        started.recv().expect("the router runtime started its listener");
        Host {
            router,
            socket,
            store_root,
            rt: Some(rt),
            _dir: dir,
        }
    }

    async fn client(&self, id: &str) -> Client {
        Client::connect_unix(&self.socket, ClientConfig::new(id, &self.store_root))
            .await
            .unwrap()
    }

    fn stop(&mut self) {
        self.router.shutdown();
        if let Some(rt) = self.rt.take() {
            // A runtime cannot be dropped from inside another one.
            std::thread::spawn(move || rt.shutdown_timeout(Duration::from_secs(2)))
                .join()
                .expect("the router runtime stopped");
        }
    }
}

/// A latest-mode consumer: extracts the frame, drops the message, reads the bytes and then
/// takes `delay` to "render" them. Returns (frames seen, coalesced, readback times).
async fn consumer(client: Client, delay: Duration) -> (u64, u64, Vec<Duration>) {
    let mut sub = client
        .subscribe("world.demo.frame", SubscriptionConfig::latest())
        .await
        .unwrap();
    let (mut seen, mut replaced, mut readback) = (0, 0, Vec::new());
    while let Some(m) = sub.next().await {
        if m.payload().get("end").is_some() {
            break;
        }
        replaced += m.replaced();
        let frame = m.artifact("frame").unwrap();
        drop(m);
        let t = Instant::now();
        let bytes = frame.read_all().await.unwrap();
        readback.push(t.elapsed());
        assert_eq!(bytes.len(), W * H * 4);
        tokio::time::sleep(delay).await;
        seen += 1;
    }
    (seen, replaced, readback)
}

async fn run(host: &Host, agents: usize) {
    let producer = host.client("producer").await;
    producer
        .declare_topic("world.demo.frame", Retained::None)
        .await
        .unwrap();
    let mut consumers = Vec::new();
    for (i, delay) in [0u64, 0, 40].into_iter().enumerate() {
        let c = host.client(&format!("consumer-{i}")).await;
        consumers.push(tokio::spawn(consumer(c, Duration::from_millis(delay))));
    }
    let mut services = Vec::new();
    for k in 0..agents {
        let c = host.client(&format!("agent-{k}")).await;
        let mut svc = c
            .register(&format!("agent.a{k}"), ServiceConfig::default())
            .await
            .unwrap();
        services.push(tokio::spawn(async move {
            let _c = c;
            while let Some(req) = svc.next().await {
                req.reply(obj(json!({})), &[]).await.unwrap();
            }
        }));
    }
    let caller = host.client("coordinator").await;
    // Let every subscription land before the first frame.
    while host.router.stats().subscriptions != 3 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let pixels: Vec<u8> = (0..W * H * 4).map(|i| (i % 253) as u8).collect();
    let frames = HZ * SECONDS;
    let period = Duration::from_nanos(1_000_000_000 / HZ);
    let (mut allocate, mut copy, mut seal) = (Vec::new(), Vec::new(), Vec::new());
    let (mut publish, mut rpc) = (Vec::new(), Vec::new());
    let (mut peak_bytes, mut peak_roots, mut peak_queued, mut late) = (0u64, 0u64, 0usize, 0u32);
    let cpu0 = proc_cpu_seconds();
    let router_cpu0 = thread_cpu_seconds(Some(ROUTER_THREAD));
    let start = Instant::now();
    for n in 0..frames {
        let deadline = start + period * n as u32;
        let t = Instant::now();
        let mut w = producer
            .artifacts()
            .allocate(pixels.len() as u64, "image/x-rgba")
            .await
            .unwrap();
        allocate.push(t.elapsed());
        let t = Instant::now();
        w.write_all(&pixels).unwrap();
        copy.push(t.elapsed());
        let t = Instant::now();
        let frame = w.seal().await.unwrap();
        seal.push(t.elapsed());
        let t = Instant::now();
        producer
            .publish(
                "world.demo.frame",
                obj(json!({"n": n})),
                &[("frame", &frame)],
            )
            .await
            .unwrap();
        publish.push(t.elapsed());
        drop(frame);
        let mut calls = Vec::new();
        for k in 0..agents {
            let caller = caller.clone();
            calls.push(tokio::spawn(async move {
                let t = Instant::now();
                caller
                    .call_and_wait(&format!("agent.a{k}"), None, "Ping", obj(json!({})), &[])
                    .await
                    .unwrap();
                t.elapsed()
            }));
        }
        for c in calls {
            rpc.push(c.await.unwrap());
        }
        let s = host.router.stats();
        peak_bytes = peak_bytes.max(s.store_bytes);
        peak_roots = peak_roots.max(s.artifact_roots);
        peak_queued = peak_queued.max(s.queued);
        let next = deadline + period;
        if Instant::now() > next {
            late += 1;
        } else {
            tokio::time::sleep_until(next.into()).await;
        }
    }
    let wall = start.elapsed().as_secs_f64();
    let cpu = proc_cpu_seconds() - cpu0;
    let router_cpu = thread_cpu_seconds(Some(ROUTER_THREAD)) - router_cpu0;
    let end = Instant::now();
    producer
        .publish("world.demo.frame", obj(json!({"end": true})), &[])
        .await
        .unwrap();
    let mut results = Vec::new();
    let mut readback = Vec::new();
    for c in consumers {
        let (seen, replaced, mut times) = c.await.unwrap();
        readback.append(&mut times);
        results.push((seen, replaced));
    }
    while host.router.stats().store_bytes != 0 {
        assert!(
            end.elapsed() < Duration::from_secs(10),
            "the store never drained: {:?}",
            host.router.stats()
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let collect_lag = end.elapsed();
    let live = host.router.stats();
    for s in services {
        s.abort();
    }

    println!("agents={agents} frames={frames} over {wall:.2}s, late frames {late}");
    println!("  {}", percentiles("allocate (quota + staging file)", &mut allocate));
    println!("  {}", percentiles("producer copy into staging", &mut copy));
    println!("  {}", percentiles("seal (router copy to a sealed inode)", &mut seal));
    println!("  {}", percentiles("publish admission", &mut publish));
    println!("  {}", percentiles("rpc round trip", &mut rpc));
    println!("  {}", percentiles("consumer readback of 1.2 MB", &mut readback));
    println!(
        "  cpu cores: router {:.3} of {ROUTER_WORKERS} threads, whole process {:.3} of {} threads on {} cpus",
        router_cpu / wall,
        cpu / wall,
        ROUTER_WORKERS + CLIENT_WORKERS,
        std::thread::available_parallelism().map_or(0, |n| n.get())
    );
    println!(
        "  VmRSS {} VmHWM {}",
        proc_status("VmRSS:"),
        proc_status("VmHWM:")
    );
    println!(
        "  store peak {:.1} MB, live after drain {} B; roots peak {peak_roots}, live {}; queued peak {peak_queued}, live {}",
        peak_bytes as f64 / 1e6,
        live.store_bytes,
        live.artifact_roots,
        live.queued
    );
    println!(
        "  collection lag after the last frame {:.1} ms",
        collect_lag.as_secs_f64() * 1000.0
    );
    println!("  consumers (frames seen, coalesced): {results:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement; run with --release -- --ignored --nocapture"]
async fn frames_at_60hz_with_three_consumers() {
    for agents in [1, 2, 4] {
        let mut host = Host::start();
        run(&host, agents).await;
        host.stop();
    }
}
