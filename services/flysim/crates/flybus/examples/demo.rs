//! A counter RPC, a pub/sub observer and a frame artifact held past its message, in one
//! process over the in-memory transport (bus-v1 section 11).
//!
//! ```text
//! cargo run -p flybus --example demo
//! ```

use std::io::Write;

use flybus::{
    Client, ClientConfig, Policy, Retained, Router, RouterConfig, ServiceConfig, SubscriptionConfig,
};
use serde_json::{Map, Value, json};

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("flybus-demo-{}", std::process::id()));
    let mut config = RouterConfig::new(&root);
    config.policy = Policy::open();
    let router = Router::new(config)?;
    let connect = |id: &str| {
        Client::connect(
            router.connect_in_memory_as(id),
            ClientConfig::new(id, &root),
        )
    };

    // A counter service.
    let counter = connect("counter").await?;
    let mut svc = counter
        .register("example.counter", ServiceConfig::default())
        .await?;
    tokio::spawn(async move {
        let mut total = 0;
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
                None,
                "Counter.Increment",
                obj(json!({"amount": 2})),
                &[],
            )
            .await?;
        println!("counter total = {}", res.outcome()["total"]);
    }

    // An observer of a frame topic.
    app.declare_topic("world.demo.frame", Retained::None)
        .await?;
    let observer = connect("observer").await?;
    let mut frames = observer
        .subscribe("world.demo.frame", SubscriptionConfig::latest())
        .await?;

    let mut writer = app
        .artifacts()
        .allocate(160 * 144 * 4, "image/x-rgba")
        .await?;
    writer.write_all(&vec![0x7f; 160 * 144 * 4])?;
    let frame = writer.seal().await?;
    let receipt = app
        .publish(
            "world.demo.frame",
            obj(json!({"width": 160, "height": 144})),
            &[("frame", &frame)],
        )
        .await?;
    println!(
        "published sequence {} to {} subscriber(s)",
        receipt.topic_sequence, receipt.subscribers
    );
    drop(frame);

    let message = frames.next().await.ok_or("subscription closed")?;
    let image = message.artifact("frame")?;
    drop(message); // the extracted handle still owns the delivery
    let bytes = image.read_all().await?;
    println!(
        "read {} bytes after the message was dropped; router: {:?}",
        bytes.len(),
        router.stats()
    );
    drop(image); // the last handle: the delivery is consumed and the frame collected

    router.shutdown();
    std::fs::remove_dir_all(&root)?;
    Ok(())
}
