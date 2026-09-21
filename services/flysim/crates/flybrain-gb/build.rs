use std::path::{Path, PathBuf};

/// Sources of binjgb's `binjgb` library target, minus the ones only its
/// Emscripten wrapper needed (`rewind.c`) and minus every host/SDL/debugger
/// translation unit. See `../../vendor/binjgb/PROVENANCE.md`.
const BINJGB_SOURCES: &[&str] = &["emulator.c", "common.c", "memory.c", "joypad.c"];

fn main() {
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = crate_dir.join("../../vendor/binjgb");
    let vendor_src = vendor.join("src");
    assert!(
        vendor_src.join("emulator.c").exists(),
        "vendored binjgb is missing: {}",
        vendor_src.display()
    );

    let mut build = cc::Build::new();
    build
        .include(&vendor_src)
        // No NDEBUG of our own: the cc crate defines it for profiles with
        // debug assertions off, so `assert(buffer->position <= buffer->end)`
        // in write_audio_frame stays live in debug builds, where a misconfigured
        // audio_frames would otherwise corrupt memory quietly.
        .flag_if_supported("-fno-strict-aliasing")
        // The same relaxations CMakeLists.txt applies to binjgb for non-MSVC
        // compilers; binjgb is warning-clean only with these.
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-implicit-fallthrough")
        .flag_if_supported("-Wno-missing-field-initializers");

    for source in BINJGB_SOURCES {
        let path = vendor_src.join(source);
        rerun(&path);
        build.file(path);
    }
    for header in ["emulator.h", "common.h", "memory.h", "joypad.h", "builtin-palettes.def"] {
        rerun(&vendor_src.join(header));
    }

    let shim = crate_dir.join("csrc/shim.c");
    rerun(&shim);
    build.file(shim);

    build.compile("flybrain_gb_binjgb");

    // sizeof(EmulatorState) is ABI dependent, so the target triple is part of
    // the save-state format id in compatibility.rs.
    println!("cargo:rustc-env=FLY_GB_TARGET={}", std::env::var("TARGET").unwrap());
}

fn rerun(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}
