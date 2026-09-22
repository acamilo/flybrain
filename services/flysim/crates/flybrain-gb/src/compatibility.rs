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
    /// The adapter's version string, e.g. `pokered-unique8-v6`.
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

/// Position of the adapter's version string in [`Compatibility::string`].
///
/// `{kernel}/{adapter}/{fingerprint}/{plasticity}/binjgb:{rev}/pokered:{commit}/statefmt:{id}`,
/// so the adapter is segment one. Nothing else in the string may move for a migration to be
/// considered: a different kernel, dataset, plasticity, emulator revision, symbol provenance or
/// state format is a different *fly*, not a different reward rule.
const ADAPTER_SEGMENT: usize = 1;

/// The environment variable that opts a deploy into the adapter migration.
///
/// Read by flysim at restore and by `infra/05-deploy.sh`'s compatibility gate. Comma- or
/// whitespace-separated adapter ids, e.g. `FLY_ACCEPT_ADAPTERS=pokered-unique8-v5`.
pub const ACCEPT_ADAPTERS_ENV: &str = "FLY_ACCEPT_ADAPTERS";

/// What a build may do with a checkpoint whose compatibility string is not its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreDecision {
    /// Byte-identical. Restore it, as every build always has.
    Exact,
    /// Every segment but the adapter's is identical, this build's adapter says it can migrate
    /// from that one, and the operator named it in [`ACCEPT_ADAPTERS_ENV`]. Restore it.
    MigrateAdapter { from: String },
    /// Refuse, and say which of the three conditions failed.
    Refuse(&'static str),
}

/// Decide whether `checkpoint`'s compatibility string may be restored under `current`.
///
/// Three conditions, all required, in the order they are cheapest to explain:
///
/// 1. the two strings differ in the adapter segment and **nowhere else**;
/// 2. `migrates_from` -- the running adapter's own list -- contains the checkpoint's adapter, so
///    the code that will read that state says out loud that it can;
/// 3. `accepted` -- [`ACCEPT_ADAPTERS_ENV`] as the operator set it for this deploy -- contains it
///    too, so no build ever migrates a run by itself.
///
/// Condition 2 without condition 3 would make the migration silent; condition 3 without condition
/// 2 would let an operator wave through a pair nobody wrote a migration for. Neither alone is
/// enough, which is why both are here.
pub fn decide(
    checkpoint: &str,
    current: &str,
    migrates_from: &[&str],
    accepted: &[String],
) -> RestoreDecision {
    if checkpoint == current {
        return RestoreDecision::Exact;
    }
    let old: Vec<&str> = checkpoint.split('/').collect();
    let new: Vec<&str> = current.split('/').collect();
    if old.len() != new.len() {
        return RestoreDecision::Refuse("the two compatibility strings do not have the same shape");
    }
    let differing: Vec<usize> = (0..old.len()).filter(|&index| old[index] != new[index]).collect();
    if differing != [ADAPTER_SEGMENT] {
        return RestoreDecision::Refuse(
            "more than the adapter version differs; nothing but a reward-rule change can migrate",
        );
    }
    let from = old[ADAPTER_SEGMENT];
    if !migrates_from.contains(&from) {
        return RestoreDecision::Refuse("this build's adapter has no migration from that adapter");
    }
    if !accepted.iter().any(|name| name == from) {
        return RestoreDecision::Refuse(
            "the checkpoint's adapter is not in FLY_ACCEPT_ADAPTERS, so the migration was not \
             asked for",
        );
    }
    RestoreDecision::MigrateAdapter { from: from.to_string() }
}

/// Parse [`ACCEPT_ADAPTERS_ENV`]: comma- or whitespace-separated, empty entries dropped.
///
/// An unset variable and an empty one are the same thing -- no migration -- so that clearing the
/// opt-in is one edit rather than two.
pub fn accepted_adapters(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
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
                "lif-1ms-f64-v2/pokered-unique8-v6/aa:bb:cc:dd:ee:ff:00/",
                "fly-kc-mbon-rstdp-v2/",
                "binjgb:c60e138da5a795ebb55e56b11b7e90024e41112c/",
                "pokered:0cd19d3b877b7dc66d12c7050bed9a7f38154d4b",
            )
        );
    }

    fn with_adapter(adapter: &'static str) -> String {
        Compatibility { adapter, ..fixture() }.string()
    }

    #[test]
    fn an_identical_string_restores_without_any_opt_in() {
        let current = with_adapter("pokered-unique8-v6");
        assert_eq!(decide(&current, &current, &[], &[]), RestoreDecision::Exact);
    }

    #[test]
    fn a_v5_checkpoint_restores_under_v6_only_with_the_opt_in() {
        let old = with_adapter("pokered-unique8-v5");
        let new = with_adapter("pokered-unique8-v6");
        let migrates = ["pokered-unique8-v5"];

        assert!(matches!(decide(&old, &new, &migrates, &[]), RestoreDecision::Refuse(_)));
        assert_eq!(
            decide(&old, &new, &migrates, &accepted_adapters(Some("pokered-unique8-v5"))),
            RestoreDecision::MigrateAdapter { from: "pokered-unique8-v5".to_string() }
        );
        // And only for a pair the running adapter says it can migrate.
        assert!(matches!(
            decide(&old, &new, &[], &accepted_adapters(Some("pokered-unique8-v5"))),
            RestoreDecision::Refuse(_)
        ));
    }

    #[test]
    fn nothing_but_the_adapter_segment_may_move() {
        let migrates = ["pokered-unique8-v5"];
        let accepted = accepted_adapters(Some("pokered-unique8-v5"));
        let new = with_adapter("pokered-unique8-v6");

        // A different dataset, with the same adapter bump, is not a migration.
        let other_dataset = Compatibility {
            adapter: "pokered-unique8-v5",
            dataset_fingerprint: "00:11:22:33:44:55:66",
            ..fixture()
        }
        .string();
        assert!(matches!(
            decide(&other_dataset, &new, &migrates, &accepted),
            RestoreDecision::Refuse(_)
        ));

        // Neither is a different kernel, and neither is a string of another shape.
        let other_kernel =
            Compatibility { adapter: "pokered-unique8-v5", neural_kernel_version: "lif-1ms-f64-v3", ..fixture() }
                .string();
        assert!(matches!(
            decide(&other_kernel, &new, &migrates, &accepted),
            RestoreDecision::Refuse(_)
        ));
        assert!(matches!(decide("a/b", &new, &migrates, &accepted), RestoreDecision::Refuse(_)));
    }

    #[test]
    fn the_opt_in_list_is_separated_by_commas_or_spaces() {
        assert!(accepted_adapters(None).is_empty());
        assert!(accepted_adapters(Some("  ")).is_empty());
        assert_eq!(
            accepted_adapters(Some("pokered-unique8-v5, pokered-unique8-v4")),
            vec!["pokered-unique8-v5".to_string(), "pokered-unique8-v4".to_string()]
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
