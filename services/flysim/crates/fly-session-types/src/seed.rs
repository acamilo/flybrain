//! `seed-derivation-v1`: independent per-agent seeds from one recorded master seed.
//!
//! The specification is `docs/design/session-framework/seed-derivation-v1.md`; this is its
//! reference implementation, and `fixtures/seed-vectors.json` its test vectors, which the
//! TypeScript package reproduces.

use sha2::{Digest as _, Sha256};

use crate::canonical;
use crate::scalar::{Result, err, is_id};

/// The algorithm identity. It is part of composition identity: changing any byte of the
/// derivation requires a new id.
pub const ALGORITHM: &str = "seed-derivation-v1";

/// The domain separation prefix hashed before the inputs.
pub const PREFIX: &str = "flybrain/seed-derivation-v1";

/// The SHA-256 of the derivation material for one agent, lowercase hex.
pub fn material_digest(master_seed: u64, agent_id: &str) -> Result<String> {
    Ok(canonical::sha256_hex(&material(master_seed, agent_id)?))
}

/// The exact bytes hashed: the prefix, the master seed as a canonical `U64` decimal string and
/// the agent id, each followed by one `\n`.
pub fn material(master_seed: u64, agent_id: &str) -> Result<Vec<u8>> {
    if !is_id(agent_id) {
        return err("seed derivation: agentId is not a valid id");
    }
    Ok(format!("{PREFIX}\n{master_seed}\n{agent_id}\n").into_bytes())
}

/// The signed 32-bit seed `Agent.Initialize` takes for `agent_id`.
///
/// The digest is read as eight big-endian `u32` lanes; the first nonzero lane becomes the
/// seed, reinterpreted as two's-complement `i32`. Skipping zero lanes keeps the seed usable
/// by an xorshift generator, whose state must not be zero. If every lane were zero the
/// material is rehashed with a counter suffix, which no observed input has needed.
pub fn agent_seed(master_seed: u64, agent_id: &str) -> Result<i32> {
    let mut material = material(master_seed, agent_id)?;
    for round in 0u32..4 {
        if round > 0 {
            material.extend_from_slice(format!("{round}\n").as_bytes());
        }
        let digest = Sha256::digest(&material);
        for lane in digest.chunks_exact(4) {
            let word = u32::from_be_bytes([lane[0], lane[1], lane[2], lane[3]]);
            if word != 0 {
                return Ok(word as i32);
            }
        }
    }
    err("seed derivation: every lane of four digests was zero")
}

/// The seeds of a whole composition, in the order the agent ids are given.
///
/// Equal ids deliberately derive equal seeds: "Identical explicit seeds are allowed only when
/// the experiment intentionally declares them" (workers-v1 section 2), so a composition with a
/// repeated agent id is refused here rather than silently sharing a seed.
pub fn composition_seeds(master_seed: u64, agent_ids: &[String]) -> Result<Vec<i32>> {
    crate::scalar::require_unique(
        agent_ids.iter().map(String::as_str),
        "seed derivation: agentIds",
    )?;
    agent_ids
        .iter()
        .map(|id| agent_seed(master_seed, id))
        .collect()
}
