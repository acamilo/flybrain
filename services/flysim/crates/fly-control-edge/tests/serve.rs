//! `flysim::serve` with `control.via = bus`: flysim leaves the control port alone, registers the
//! control services on its embedded router, and the edge process serves the port from them.
//! This is the wiring both runtimes share (`flysim` and `flysim-session` both run `serve`).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use flysim::config::{Config, ControlVia};
use flysim::simloop::Command;
use flysim::snapshot::FeedStatus;

#[test]
fn serve_in_control_bus_mode_leaves_the_port_to_the_edge() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.feed.bind = common::free_port();
    config.control.bind = common::free_port();
    config.control.via = ControlVia::Bus;
    config.feed.bus_dir = dir.path().join("bus");
    let control_bind = config.control.bind;

    let stop = Arc::new(AtomicBool::new(false));
    let serving = {
        let config = config.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            flysim::serve(
                config,
                move |shared, _snapshots, mut commands, _notifier| {
                    // A stand-in loop: beats, answers pauses, stops when told.
                    while !stop.load(Ordering::SeqCst) {
                        shared.beat();
                        while let Ok(command) = commands.try_recv() {
                            if let Command::Pause { reply } = command {
                                let _ = reply.send(FeedStatus::Paused);
                            }
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(())
                },
            )
        })
    };

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let request = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
        // The router's sockets appear; flysim never binds the control port.
        let started = Instant::now();
        while !dir.path().join("bus/control-edge.sock").exists() {
            assert!(started.elapsed() < Duration::from_secs(20), "no control socket");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(common::raw_http(control_bind, request).await.is_none(), "flysim bound :7401");
        for role in ["stage", "bridge", "watchdog", "operator"] {
            assert!(dir.path().join(format!("bus/control/{role}.sock")).exists(), "{role}");
        }
        // The feed is still direct: no feed socket.
        assert!(!dir.path().join("bus/edge.sock").exists());

        let edge_config = fly_control_edge::EdgeConfig::from_flysim(&config, None);
        let metrics = Arc::new(fly_control_edge::EdgeMetrics::default());
        let edge = tokio::spawn(fly_control_edge::run(edge_config, Arc::clone(&metrics)));
        let started = Instant::now();
        while metrics.connected.load(Ordering::Relaxed) == 0 {
            assert!(started.elapsed() < Duration::from_secs(20), "the edge never bound");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let health = common::raw_http(control_bind, request).await.unwrap();
        assert!(health.starts_with(b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&health));
        let pause = "POST /pause HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let paused = String::from_utf8(common::raw_http(control_bind, pause).await.unwrap()).unwrap();
        assert!(paused.starts_with("HTTP/1.1 200"), "{paused}");
        assert!(paused.ends_with("{\"status\":\"paused\"}"), "{paused}");

        // flysim stops: the edge lets go of the port.
        stop.store(true, Ordering::SeqCst);
        let started = Instant::now();
        while common::raw_http(control_bind, request).await.is_some() {
            assert!(started.elapsed() < Duration::from_secs(20), ":7401 stayed bound");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        edge.abort();
    });
    serving.join().unwrap().unwrap();
}
