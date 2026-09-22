//! bus-v1 section 11 item 7, as a measurement rather than a gate: 640x480 RGBA frames at
//! 60 Hz over a Unix socket to three consumers (one delayed), with 1, 2 and 4 agent services
//! pinged every frame. Router and clients share this process, so CPU and RSS are the whole
//! process. Run with:
//!
//! ```text
//! cargo test --release -p flybus --test perf -- --ignored --nocapture
//! ```

mod common;

use std::io::Write;
use std::time::{Duration, Instant};

use common::{Via, env, obj};
use flybus::{Client, Retained, ServiceConfig, SubscriptionConfig};
use serde_json::json;

const W: usize = 640;
const H: usize = 480;
const HZ: u64 = 60;
const SECONDS: u64 = 2;

fn proc_cpu_seconds() -> f64 {
    // utime + stime, fields 14 and 15 of /proc/self/stat, in clock ticks (100 Hz on Linux).
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (ticks(11) + ticks(12)) / 100.0
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

async fn consumer(client: Client, delay: Duration) -> (u64, u64) {
    let mut sub = client
        .subscribe("world.demo.frame", SubscriptionConfig::latest())
        .await
        .unwrap();
    let (mut seen, mut replaced) = (0, 0);
    while let Some(m) = sub.next().await {
        if m.payload().get("end").is_some() {
            break;
        }
        replaced += m.replaced();
        let frame = m.artifact("frame").unwrap();
        drop(m);
        let bytes = frame.read_all().await.unwrap();
        assert_eq!(bytes.len(), W * H * 4);
        tokio::time::sleep(delay).await;
        seen += 1;
    }
    (seen, replaced)
}

async fn run(agents: usize) {
    let e = env(Via::Unix).await;
    let producer = e.client("producer").await;
    producer
        .declare_topic("world.demo.frame", Retained::None)
        .await
        .unwrap();
    let mut consumers = Vec::new();
    for (i, delay) in [0u64, 0, 40].into_iter().enumerate() {
        let c = e.client(&format!("consumer-{i}")).await;
        consumers.push(tokio::spawn(consumer(c, Duration::from_millis(delay))));
    }
    let mut services = Vec::new();
    for k in 0..agents {
        let c = e.client(&format!("agent-{k}")).await;
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
    let caller = e.client("coordinator").await;
    // Let every subscription land before the first frame.
    e.settle("subscribed", |s| s.subscriptions == 3).await;

    let pixels: Vec<u8> = (0..W * H * 4).map(|i| (i % 253) as u8).collect();
    let frames = HZ * SECONDS;
    let period = Duration::from_nanos(1_000_000_000 / HZ);
    let (mut produce, mut publish, mut rpc) = (Vec::new(), Vec::new(), Vec::new());
    let (mut peak_bytes, mut peak_roots, mut peak_queued, mut late) = (0u64, 0u64, 0usize, 0u32);
    let cpu0 = proc_cpu_seconds();
    let start = Instant::now();
    for n in 0..frames {
        let deadline = start + period * n as u32;
        let t = Instant::now();
        let mut w = producer
            .artifacts()
            .allocate(pixels.len() as u64, "image/x-rgba")
            .await
            .unwrap();
        w.write_all(&pixels).unwrap();
        let frame = w.seal().await.unwrap();
        produce.push(t.elapsed());
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
        let s = e.stats();
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
    let end = Instant::now();
    producer
        .publish("world.demo.frame", obj(json!({"end": true})), &[])
        .await
        .unwrap();
    let mut results = Vec::new();
    for c in consumers {
        results.push(c.await.unwrap());
    }
    e.settle("collected", |s| s.store_bytes == 0).await;
    let collect_lag = end.elapsed();
    for s in services {
        s.abort();
    }
    for v in [&mut produce, &mut publish, &mut rpc] {
        v.sort();
    }
    let ms = |d: Duration| format!("{:.2}", d.as_secs_f64() * 1000.0);
    println!("agents={agents} frames={frames} over {wall:.2}s, late frames {late}");
    println!(
        "  produce (allocate+write+seal copy) ms p50/p95/p99: {}/{}/{}",
        ms(pct(&produce, 0.5)),
        ms(pct(&produce, 0.95)),
        ms(pct(&produce, 0.99))
    );
    println!(
        "  publish admission ms p50/p95/p99: {}/{}/{}",
        ms(pct(&publish, 0.5)),
        ms(pct(&publish, 0.95)),
        ms(pct(&publish, 0.99))
    );
    println!(
        "  rpc round trip ms p50/p95/p99: {}/{}/{}",
        ms(pct(&rpc, 0.5)),
        ms(pct(&rpc, 0.95)),
        ms(pct(&rpc, 0.99))
    );
    println!(
        "  process cpu {:.2} cores; VmRSS {} VmHWM {}",
        cpu / wall,
        proc_status("VmRSS:"),
        proc_status("VmHWM:")
    );
    println!(
        "  store peak {:.1} MB, peak roots {peak_roots}, peak queued {peak_queued}, drain+collect {:.1} ms",
        peak_bytes as f64 / 1e6,
        collect_lag.as_secs_f64() * 1000.0
    );
    println!("  consumers (frames seen, replaced): {results:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement; run with --release -- --ignored --nocapture"]
async fn frames_at_60hz_with_three_consumers() {
    for agents in [1, 2, 4] {
        run(agents).await;
    }
}
