//! The guide's first deliverable: a counter RPC, a pub/sub observer and a frame artifact held
//! past its message object's lifetime, in one program (bus-v1 section 11, implementation
//! guide section 1). No game, browser or second transport is involved.
//!
//! ```text
//! cargo run -p flybus --example demo
//! ```
//!
//! `tests/example_demo.rs` runs [`run`] and asserts every line it returns.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use flybus::{
    Client, ClientConfig, Policy, Retained, Router, RouterConfig, ServiceConfig,
    SubscriptionConfig,
};
use serde_json::{Map, Value, json};

const W: usize = 160;
const H: usize = 144;

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// The three parts, in one program, over one router. Returns the lines the example prints.
pub async fn run() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    static RUNS: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "flybus-demo-{}-{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    let mut config = RouterConfig::new(&root);
    config.policy = Policy::open();
    let router = Router::new(config)?;
    let connect = |id: &str| {
        Client::connect(
            router.connect_in_memory_as(id),
            ClientConfig::new(id, &root),
        )
    };
    let mut lines = Vec::new();

    // 1. A counter service. An exclusive endpoint, pinned by its caller to the registration
    //    it discovered, reached through the router like every other operation.
    let counter = connect("counter").await?;
    let mut svc = counter
        .register("example.counter", ServiceConfig::default())
        .await?;
    let incarnation = svc.incarnation().to_owned();
    let service = tokio::spawn(async move {
        let mut total = 0i64;
        while let Some(req) = svc.next().await {
            total += req.payload()["amount"].as_i64().unwrap_or(0);
            let _ = req.reply(obj(json!({ "total": total })), &[]).await;
        }
    });
    let app = connect("app").await?;
    for _ in 0..3 {
        let res = app
            .call_and_wait(
                "example.counter",
                Some(&incarnation),
                "Counter.Increment",
                obj(json!({"amount": 1})),
                &[],
            )
            .await?;
        lines.push(format!("counter total = {}", res.outcome()["total"]));
    }

    // 2. A pub/sub observer. A latest-value subscription, so a slow observer coalesces
    //    instead of holding the producer up.
    app.declare_topic("world.demo.frame", Retained::None).await?;
    let observer = connect("observer").await?;
    let mut frames = observer
        .subscribe("world.demo.frame", SubscriptionConfig::latest())
        .await?;

    // 3. A frame artifact. The bytes live in the store; the message carries a reference and
    //    the dimensions.
    let mut writer = app
        .artifacts()
        .allocate((W * H * 4) as u64, "image/x-rgba")
        .await?;
    writer.write_all(&vec![0x7f; W * H * 4])?;
    let frame = writer.seal().await?;
    let receipt = app
        .publish(
            "world.demo.frame",
            obj(json!({"width": W, "height": H})),
            &[("frame", &frame)],
        )
        .await?;
    lines.push(format!(
        "published sequence {} to {} subscriber(s)",
        receipt.topic_sequence, receipt.subscribers
    ));
    // The producer lets go of its own hold; the delivery keeps the bytes alive. The release
    // travels the control lane like any other operation, so the count below waits for it
    // instead of reading a number that may still include it.
    drop(frame);
    let released = Instant::now() + Duration::from_secs(10);
    while router.stats().artifact_roots > 1 {
        if Instant::now() > released {
            return Err("the producer's own hold was never released".into());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    let message = frames.next().await.ok_or("the subscription closed")?;
    let image = message.artifact("frame")?;
    drop(message); // the extracted handle still owns the delivery
    let bytes = image.read_all().await?;
    lines.push(format!(
        "read {} bytes after the message was dropped",
        bytes.len()
    ));
    let held = router.stats();
    lines.push(format!(
        "while the frame is held: {} artifact(s), {} root(s)",
        held.sealed_artifacts, held.artifact_roots
    ));
    drop(image); // the last handle: the delivery is consumed and the frame collected

    // Consumption reaches the router on the client's control lane, so collection is not
    // instantaneous.
    let deadline = Instant::now() + Duration::from_secs(10);
    while router.stats().artifacts > 0 {
        if Instant::now() > deadline {
            return Err("the frame was never collected".into());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let collected = router.stats();
    lines.push(format!(
        "after the last handle: {} artifact(s), {} root(s)",
        collected.artifacts, collected.artifact_roots
    ));

    service.abort();
    router.shutdown();
    drop((app, observer, counter));
    let _ = std::fs::remove_dir_all(&root);
    Ok(lines)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in run().await? {
        println!("{line}");
    }
    Ok(())
}
