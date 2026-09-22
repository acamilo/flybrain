//! The guide's example is also a test: `cargo run -p flybus --example demo` prints exactly
//! these lines (bus-v1 section 11, implementation guide section 1).

#[allow(dead_code)]
#[path = "../examples/demo.rs"]
mod demo;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_example_shows_a_counter_rpc_an_observer_and_a_held_frame() {
    let lines = demo::run().await.expect("the example ran");
    assert_eq!(
        lines.iter().map(String::as_str).collect::<Vec<_>>(),
        vec![
            // A counter service, called three times through the router.
            "counter total = 1",
            "counter total = 2",
            "counter total = 3",
            // One observer, one accepted publication, one sequence number.
            "published sequence 1 to 1 subscriber(s)",
            // 160x144 RGBA, read after the message object was dropped.
            "read 92160 bytes after the message was dropped",
            "while the frame is held: 1 artifact(s), 1 root(s)",
            "after the last handle: 0 artifact(s), 0 root(s)",
        ]
    );
}
