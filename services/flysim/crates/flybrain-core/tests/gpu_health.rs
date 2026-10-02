//! The GPU fallback record (`lif::health`): one failure makes the GPU unusable for the process,
//! it is counted and logged once per failure, a marker file carries it to sibling worker
//! processes, and the metric text names the backend that runs.

use flybrain_core::lif::LifBackend;
use flybrain_core::lif::health;

#[test]
fn a_failure_marks_the_gpu_unusable_and_is_exported() {
    // The only test in this binary: it owns the process-wide record and the environment.
    health::reset_for_tests();
    assert!(!health::gpu_unusable());
    assert_eq!(health::fallbacks(), 0);

    let cuda = LifBackend::Cuda { device: 0 };
    let before = health::render_prometheus(cuda, cuda);
    assert!(before.contains("fly_brain_backend{backend=\"cuda\"} 1"));
    assert!(before.contains("fly_brain_backend{backend=\"cpu\"} 0"));
    assert!(before.contains("fly_brain_backend_fallbacks_total 0"));

    health::record_fallback("device lost\nsecond line");
    assert!(health::gpu_unusable());
    assert_eq!(health::fallbacks(), 1);
    assert_eq!(health::last_reason().as_deref(), Some("device lost second line"));
    let after = health::render_prometheus(LifBackend::Cpu, cuda);
    assert!(after.contains("fly_brain_backend{backend=\"cpu\"} 1"));
    assert!(after.contains("fly_brain_backend{backend=\"cuda\"} 0"));
    assert!(after.contains("fly_brain_backend_wanted 1"));
    assert!(after.contains("fly_brain_backend_fallbacks_total 1"));

    // With a marker the file is the record: a sibling process's failure counts here, and
    // resetting it (a new launcher) ends the fallback.
    let dir = tempfile::tempdir().expect("tmp");
    let marker = dir.path().join("lif-gpu-fallbacks");
    let prom = dir.path().join("fly_brain_backend.prom");
    // SAFETY: single test in this binary, no other thread reads the environment.
    unsafe {
        std::env::set_var(health::MARKER_ENV, &marker);
        std::env::set_var(health::PROM_ENV, &prom);
    }
    assert!(!health::gpu_unusable(), "an empty marker is a clean slate");
    std::fs::write(&marker, "from a worker process\n").expect("marker");
    assert!(health::gpu_unusable());
    assert_eq!(health::fallbacks(), 1);
    health::record_fallback("second");
    assert_eq!(health::fallbacks(), 2);
    assert_eq!(health::marker_fallbacks(&marker), 2);
    health::write_textfile(LifBackend::Cpu, cuda);
    let text = std::fs::read_to_string(&prom).expect("textfile");
    assert!(text.contains("fly_brain_backend_fallbacks_total 2"), "{text}");
    health::reset_marker(&marker);
    assert!(!health::gpu_unusable());
    unsafe {
        std::env::remove_var(health::MARKER_ENV);
        std::env::remove_var(health::PROM_ENV);
    }
}
