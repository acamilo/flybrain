//! Golden scenario `versions`: version strings for default and non-default configurations.
//!
//! A non-default configuration hashes its numeric parameters joined with `,`, so these assertions
//! pin the FNV-1a-32 hash, the frozen parameter order, and the JavaScript number formatting that
//! produces the string being hashed (`0.005`, not `5e-3`).

mod common;

use common::golden;
use flybrain_core::json::JsonValue;
use flybrain_core::lif::{kernel_version, LifConfig, Stimulation, NEURAL_KERNEL_VERSION};
use flybrain_core::plasticity::{plasticity_version, PlasticityConfig, PLASTICITY_VERSION};
use flybrain_core::retina::RetinaConfig;

/// The label -> version pairs the generator wrote, as a lookup.
fn expected(golden: &common::Golden, path: &str) -> Vec<(String, String)> {
    golden
        .at(path)
        .as_array()
        .expect("an array of {label, version}")
        .iter()
        .map(|entry| {
            (
                entry
                    .get("label")
                    .and_then(JsonValue::as_str)
                    .unwrap()
                    .to_string(),
                entry
                    .get("version")
                    .and_then(JsonValue::as_str)
                    .unwrap()
                    .to_string(),
            )
        })
        .collect()
}

fn lif_patch(label: &str) -> LifConfig {
    let mut config = LifConfig::default();
    match label {
        "default" => {}
        "decayMs25" => config.decay_ms = 25.0,
        "threshold0_9" => config.threshold = 0.9,
        "refractory3" => config.refractory_ms = 3,
        "synapseScale0_006" => config.synapse_scale = 0.006,
        "baselineMax0_05" => config.baseline_max = 0.05,
        "noiseKicks200" => config.noise_kicks = 200,
        "noiseAmount0_5" => config.noise_amount = 0.5,
        "rateAlpha1over50" => config.rate_alpha = 1.0 / 50.0,
        "membraneFloorMinus3" => config.membrane_floor = -3.0,
        "seed1" => config.seed = 1,
        "stimulationDrive0_3" => {
            config.stimulation = Stimulation {
                role: "reward_pam".to_string(),
                drive: 0.3,
            }
        }
        "retinaGain0_3" => {
            config.retina = RetinaConfig {
                gain: 0.3,
                width: 160,
                height: 144,
            }
        }
        "retinaWidth320" => {
            config.retina = RetinaConfig {
                gain: 0.20,
                width: 320,
                height: 144,
            }
        }
        "retinaHeight288" => {
            config.retina = RetinaConfig {
                gain: 0.20,
                width: 160,
                height: 288,
            }
        }
        // Role names are dataset labels, not kernel constants: they never move the version.
        "roleNamesOnly" => {
            config.stimulation = Stimulation {
                role: "other".to_string(),
                drive: 0.20,
            };
            config.rate_roles = Some(vec!["command_0".to_string()]);
        }
        other => panic!("unhandled lif patch {other}"),
    }
    config
}

fn plasticity_patch(label: &str) -> PlasticityConfig {
    let mut config = PlasticityConfig::default();
    match label {
        "default" => {}
        "traceMs4000" => config.trace_ms = 4000.0,
        "pairMs30" => config.pair_ms = 30.0,
        "pairWindow50" => config.pair_window_ms = 50.0,
        "potentiation0_2" => config.potentiation = 0.2,
        "depression0_1" => config.depression = 0.1,
        "learningRate0_004" => config.learning_rate = 0.004,
        "restoring0_0002" => config.restoring = 0.0002,
        "minGain0_8" => config.min_gain = 0.8,
        "maxGain1_2" => config.max_gain = 1.2,
        // preRole, postRole and budget select edges (the topology hash covers that) and are
        // deliberately absent from the version string.
        "siteAndBudgetOnly" => {
            config.pre_role = "mbon".to_string();
            config.post_role = "motor".to_string();
            config.budget = 4;
        }
        other => panic!("unhandled plasticity patch {other}"),
    }
    config
}

#[test]
fn kernel_version_strings_match_the_oracle() {
    let golden = golden("versions");
    let pairs = expected(&golden, "lif");
    assert!(pairs.len() >= 16);
    let mut derived = std::collections::HashSet::new();
    for (label, want) in &pairs {
        let got = kernel_version(&lif_patch(label));
        assert_eq!(&got, want, "kernelVersion({label})");
        if label == "default" || label == "roleNamesOnly" {
            assert_eq!(got, NEURAL_KERNEL_VERSION);
        } else {
            assert!(
                got.starts_with(&format!("{NEURAL_KERNEL_VERSION}:"))
                    && got.len() == NEURAL_KERNEL_VERSION.len() + 9,
                "{label} produced {got}"
            );
            derived.insert(got);
        }
    }
    assert_eq!(
        derived.len(),
        pairs.len() - 2,
        "each numeric parameter must reach the version hash"
    );
}

#[test]
fn plasticity_version_strings_match_the_oracle() {
    let golden = golden("versions");
    let pairs = expected(&golden, "plasticity");
    assert!(pairs.len() >= 11);
    let mut derived = std::collections::HashSet::new();
    for (label, want) in &pairs {
        let got = plasticity_version(&plasticity_patch(label));
        assert_eq!(&got, want, "plasticityVersion({label})");
        if label == "default" || label == "siteAndBudgetOnly" {
            assert_eq!(got, PLASTICITY_VERSION);
        } else {
            // Note the derived prefix is `rstdp-v2:`, not the full default string.
            assert!(
                got.starts_with("rstdp-v2:") && got.len() == 17,
                "{label} produced {got}"
            );
            derived.insert(got);
        }
    }
    assert_eq!(
        derived.len(),
        pairs.len() - 2,
        "each rule parameter must reach the version hash"
    );
}
