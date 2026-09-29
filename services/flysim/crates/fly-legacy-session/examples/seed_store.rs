//! Seed a FLYSIM01 store with one checkpoint file, as its own generation (a soak or rehearsal
//! starting from a pulled stream checkpoint; SERVE-01).
//!
//! ```sh
//! cargo run --release -p fly-legacy-session --example seed_store -- <file.checkpoint> <durable-dir>
//! ```

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, file, dir] = args.as_slice() else {
        eprintln!("usage: seed_store <file.checkpoint> <durable-dir>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(file).expect("reading the checkpoint");
    let checkpoint = flysim::store::decode(&bytes).expect("a FLYSIM01 checkpoint");
    let generation = checkpoint.runtime.generation;
    let store = flysim::store::Store::new(std::path::Path::new(dir), 2);
    store.create().expect("creating the store");
    store
        .commit(generation, &bytes, None)
        .expect("committing the checkpoint");
    println!(
        "seeded {dir} with generation {generation} (frame {}, compatibility {})",
        checkpoint.runtime.emulator_frame, checkpoint.runtime.compatibility
    );
}
