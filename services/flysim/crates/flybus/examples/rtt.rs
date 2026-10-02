//! Round-trip latency of one RPC through the router, the cost every session transition pays
//! several times (BUS-02). The router and the caller share one process (as the session's
//! router and coordinator do); the service is a child process on a Unix socket (process mode)
//! or a task in the same process over an in-memory pipe (`--in-memory`).
//!
//! ```text
//! cargo run --release -p flybus --example rtt -- [--calls N] [--payload BYTES] [--reply-artifact BYTES]
//!     [--router-workers N] [--in-memory] [--read]
//! ```
//!
//! `--reply-artifact` makes the service seal an artifact of that size into every reply (a
//! writer allocated ahead, as the session's writer pool does) and `--read` makes the caller read
//! it. Prints p50/p90/p99 in microseconds. Local synthetic timings, no capacity claim.

use std::time::{Duration, Instant};

use flybus::{Client, ClientConfig, Policy, Router, RouterConfig, ServiceConfig};
use serde_json::{Map, Value};

#[derive(Clone, Copy)]
struct Opts {
    calls: usize,
    payload: usize,
    reply_artifact: usize,
    router_workers: usize,
    in_memory: bool,
    read: bool,
    spawned: bool,
    sealing: bool,
}

fn opts(args: &[String]) -> Opts {
    let mut o = Opts {
        calls: 2000,
        payload: 512,
        reply_artifact: 0,
        router_workers: 2,
        in_memory: false,
        read: false,
        spawned: false,
        sealing: false,
    };
    let mut i = 0;
    while i < args.len() {
        let next = |i: usize| args.get(i + 1).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        match args[i].as_str() {
            "--calls" => {
                o.calls = next(i);
                i += 1;
            }
            "--payload" => {
                o.payload = next(i);
                i += 1;
            }
            "--reply-artifact" => {
                o.reply_artifact = next(i);
                i += 1;
            }
            "--router-workers" => {
                o.router_workers = next(i).max(1);
                i += 1;
            }
            "--in-memory" => o.in_memory = true,
            "--read" => o.read = true,
            "--spawned" => o.spawned = true,
            "--sealing" => o.sealing = true,
            _ => {}
        }
        i += 1;
    }
    o
}

fn payload(bytes: usize) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("blob".into(), Value::String("x".repeat(bytes)));
    m
}

async fn serve(client: Client, reply_artifact: usize, sealing: bool) {
    let mut svc = client.register("bench.echo", ServiceConfig::default()).await.unwrap();
    let mut ahead = None;
    while let Some(req) = svc.next().await {
        if sealing && reply_artifact > 0 {
            // Sealed by the router as it admits the reply; the staging file comes back as the
            // next writer (`--sealing`).
            let mut w = match ahead.take() {
                Some(w) => w,
                None => client
                    .artifacts()
                    .allocate(reply_artifact as u64, "application/octet-stream")
                    .await
                    .unwrap(),
            };
            use std::io::Write;
            w.write_all(&vec![7u8; reply_artifact]).unwrap();
            let unsealed = w.into_unsealed();
            let blob = unsealed.artifact().clone();
            let out = req
                .responder()
                .reply_sealing(payload(64), &[("blob", &blob)], vec![unsealed], true)
                .await
                .unwrap();
            ahead = out.writers.into_iter().next();
            continue;
        }
        let mut atts = Vec::new();
        if reply_artifact > 0 {
            let mut w = match ahead.take() {
                Some(w) => w,
                None => client
                    .artifacts()
                    .allocate(reply_artifact as u64, "application/octet-stream")
                    .await
                    .unwrap(),
            };
            use std::io::Write;
            w.write_all(&vec![7u8; reply_artifact]).unwrap();
            atts.push(("blob", w.seal().await.unwrap()));
        }
        let refs: Vec<(&str, &flybus::Artifact)> = atts.iter().map(|(n, a)| (*n, a)).collect();
        let _ = req.reply(payload(64), &refs).await;
        drop(req);
        if reply_artifact > 0 {
            ahead = Some(
                client
                    .artifacts()
                    .allocate(reply_artifact as u64, "application/octet-stream")
                    .await
                    .unwrap(),
            );
        }
    }
}

fn child(sock: &str, root: &str, reply_artifact: usize, sealing: bool) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    // As a session participant serves: on the runtime's one worker, not the blocked main thread.
    let (sock, root) = (sock.to_owned(), root.to_owned());
    let handle = rt.handle().clone();
    rt.block_on(async move {
        handle
            .spawn(async move {
                let client = Client::connect_unix(sock, ClientConfig::new("svc", root)).await.unwrap();
                serve(client, reply_artifact, sealing).await;
            })
            .await
            .unwrap();
    });
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--child") {
        child(&args[1], &args[2], args[3].parse().unwrap(), args[4] == "1");
        return;
    }
    let o = opts(&args);
    let dir = std::env::temp_dir().join(format!("flybus-rtt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("store");
    let sock = dir.join("bus.sock");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(o.router_workers)
        .enable_all()
        .build()
        .unwrap();
    let spawned = o.spawned;
    let (root2, sock2) = (root.clone(), sock.clone());
    let body = async move {
        let (root, sock) = (root2, sock2);
        let mut child_proc = None;
        let mut config = RouterConfig::new(&root);
        config.policy = Policy::open();
        let router = Router::new(config).unwrap();
        let _listener = router.listen_unix(&sock).await.unwrap();
        if o.in_memory {
            let c = Client::connect(router.connect_in_memory(), ClientConfig::new("svc", &root))
                .await
                .unwrap();
            tokio::spawn(serve(c, o.reply_artifact, o.sealing));
        } else {
            child_proc = Some(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--child",
                        sock.to_str().unwrap(),
                        root.to_str().unwrap(),
                        &o.reply_artifact.to_string(),
                        if o.sealing { "1" } else { "0" },
                    ])
                    .spawn()
                    .unwrap(),
            );
        }
        let caller = Client::connect(router.connect_in_memory(), ClientConfig::new("caller", &root))
            .await
            .unwrap();
        // Wait for the service to register.
        loop {
            match caller.call_and_wait("bench.echo", None, "Echo", payload(8), &[]).await {
                Ok(_) => break,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        let mut samples = Vec::with_capacity(o.calls);
        for i in 0..o.calls + 100 {
            let t = Instant::now();
            let result = caller
                .call_and_wait("bench.echo", None, "Echo", payload(o.payload), &[])
                .await
                .unwrap();
            if o.read && o.reply_artifact > 0 {
                let a = result.artifact("blob").unwrap();
                let bytes = a.read_all().await.unwrap();
                assert_eq!(bytes.len(), o.reply_artifact);
            }
            drop(result);
            if i >= 100 {
                samples.push(t.elapsed());
            }
        }
        samples.sort();
        let p = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e6;
        println!(
            "rtt mode={} sealing={} payload={} reply_artifact={} read={} router_workers={} calls={}: p50={:.0}us p90={:.0}us p99={:.0}us",
            if o.in_memory { "in-memory" } else { "process" },
            o.sealing,
            o.payload,
            o.reply_artifact,
            o.read,
            o.router_workers,
            o.calls,
            p(0.5),
            p(0.9),
            p(0.99)
        );
        child_proc
    };
    // `--spawned`: the caller runs on a runtime worker, beside the router's tasks, instead of on
    // the thread that blocks on the runtime.
    let child_proc = if spawned {
        let handle = rt.handle().clone();
        rt.block_on(async move { handle.spawn(body).await.unwrap() })
    } else {
        rt.block_on(body)
    };
    if let Some(mut c) = child_proc {
        let _ = c.kill();
        let _ = c.wait();
    }
    let _ = std::fs::remove_dir_all(&dir);
}
