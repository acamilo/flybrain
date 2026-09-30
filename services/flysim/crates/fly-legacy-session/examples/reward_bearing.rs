//! `reward_bearing IN OUT`: a copy of a FLYSIM01 checkpoint with its adapter ledger thinned so the
//! real brain earns rewards where the fly already stands -- the current map unvisited (an `AREA`
//! payout on the first sample), its exits unfound (`boundary`), its ground unwalked
//! (`exploration`), no wild win a replay (`battle`). TASK-01's brain arm
//! (`tests/session_trace.rs`), as a file for SHADOW-01's rehearsal (`tools/shadow-rehearsal.sh`),
//! where the real service restores it. A test fixture, never a stream checkpoint.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: reward_bearing IN OUT");
        std::process::exit(2);
    };
    let bytes = std::fs::read(input).expect("the checkpoint reads");
    let mut checkpoint = flysim::store::decode(&bytes).expect("FLYSIM01");
    let reward = &mut checkpoint.runtime.reward;
    let map = reward["location"]
        .as_str()
        .and_then(|l| l.split(':').next())
        .unwrap_or("0")
        .to_owned();
    if let Some(seen) = reward["seen"].as_array_mut() {
        seen.retain(|key| {
            let key = key.as_str().unwrap_or("");
            key != format!("map:{map}") && !key.starts_with(&format!("boundary:{map}:"))
        });
    }
    if let Some(tiles) = reward["tiles"].as_array_mut() {
        tiles.retain(|tile| !tile.as_str().unwrap_or("").starts_with(&format!("{map}:")));
    }
    if let Some(counts) = reward["tileCounts"].as_object_mut() {
        counts.remove(&map);
    }
    reward["wildWins"] = serde_json::json!({});
    reward["replayBlocked"] = serde_json::json!([]);
    let out = flysim::store::encode(&checkpoint.agent, &checkpoint.runtime).expect("encodes");
    std::fs::write(output, out).expect("the copy writes");
    eprintln!("thinned map {map}: {output}");
}
