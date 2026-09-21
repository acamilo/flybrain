//! The checkpoint compatibility string.
//!
//! A port of the prototype's `src/runtime/compatibility.ts`, which produced
//!
//! ```text
//! {kernel}/{adapter}/{fingerprint}/{plasticity}/binjgb:{rev}/pokered:{commit}
//! ```
//!
//! with one segment appended. All of these are build constants; nothing here
//! performs a runtime lookup. ROM identity is checked separately, against
//! [`crate::pokemon_red::SUPPORTED_ROM`].
//!
//! The kernel and plasticity version strings belong to the neural library, so
//! they are parameters: this crate must not depend on `flybrain-core`. The
//! prototype's values were `lif-1ms-f64-v2` and `fly-kc-mbon-rstdp-v2`.

use crate::emulator::Emulator;

/// binjgb revision vendored under `services/flysim/vendor/binjgb`.
pub const BINJGB_REVISION: &str = "c60e138da5a795ebb55e56b11b7e90024e41112c";

/// The prototype's neural kernel version, for reference and for the tests that
/// compare a Rust string against a TypeScript checkpoint.
pub const PROTOTYPE_NEURAL_KERNEL_VERSION: &str = "lif-1ms-f64-v2";
/// The prototype's plasticity version, same purpose.
pub const PROTOTYPE_PLASTICITY_VERSION: &str = "fly-kc-mbon-rstdp-v2";

/// Everything that must match exactly before a checkpoint may be restored.
#[derive(Debug, Clone, Copy)]
pub struct Compatibility<'a> {
    /// `kernelVersion(config)` from the neural library.
    pub neural_kernel_version: &'a str,
    /// The adapter's version string, e.g. `pokered-unique8-v5`.
    pub adapter: &'a str,
    /// The dataset's seven SHA-256 digests joined with `:`.
    pub dataset_fingerprint: &'a str,
    /// `plasticityVersion(config)` from the neural library.
    pub plasticity_version: &'a str,
    /// The game's symbol provenance, e.g. the pokered commit.
    pub pokered_commit: &'a str,
}

impl Compatibility<'_> {
    /// The prototype's string, byte for byte. Useful for reading a checkpoint
    /// the TypeScript build wrote.
    pub fn prototype_string(&self) -> String {
        format!(
            "{}/{}/{}/{}/binjgb:{}/pokered:{}",
            self.neural_kernel_version,
            self.adapter,
            self.dataset_fingerprint,
            self.plasticity_version,
            BINJGB_REVISION,
            self.pokered_commit,
        )
    }

    /// The prototype's string plus a `statefmt:` segment.
    ///
    /// `emulator_write_state` is a `memcpy` of binjgb's `EmulatorState`, so its
    /// bytes depend on the compiler's layout for that struct: the WASM build's
    /// state is a different size from a native x86-64 build's. The extra
    /// segment makes that a compatibility mismatch instead of a silent
    /// misparse. See the crate README, "State format".
    pub fn string(&self) -> String {
        format!("{}/statefmt:{}", self.prototype_string(), state_format_id())
    }
}

/// `<state size>-<target triple>`: the two things that decide whether a
/// binjgb save state written elsewhere can be memcpy'd back in here.
pub fn state_format_id() -> String {
    format!("{}-{}", Emulator::state_size(), env!("FLY_GB_TARGET"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "aa:bb:cc:dd:ee:ff:00";

    fn fixture() -> Compatibility<'static> {
        Compatibility {
            neural_kernel_version: PROTOTYPE_NEURAL_KERNEL_VERSION,
            adapter: crate::pokemon_red::REWARD_ADAPTER,
            dataset_fingerprint: FINGERPRINT,
            plasticity_version: PROTOTYPE_PLASTICITY_VERSION,
            pokered_commit: crate::pokemon_red::symbols::POKERED_COMMIT,
        }
    }

    #[test]
    fn the_prototype_segment_order_is_preserved() {
        assert_eq!(
            fixture().prototype_string(),
            concat!(
                "lif-1ms-f64-v2/pokered-unique8-v5/aa:bb:cc:dd:ee:ff:00/",
                "fly-kc-mbon-rstdp-v2/",
                "binjgb:c60e138da5a795ebb55e56b11b7e90024e41112c/",
                "pokered:0cd19d3b877b7dc66d12c7050bed9a7f38154d4b",
            )
        );
    }

    #[test]
    fn the_state_format_segment_is_appended_not_interleaved() {
        let full = fixture().string();
        assert!(full.starts_with(&fixture().prototype_string()));
        assert!(full.ends_with(&format!("/statefmt:{}", state_format_id())));
        assert!(state_format_id().contains(&Emulator::state_size().to_string()));
    }
}
