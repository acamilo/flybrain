//! `fly-scorecard`: run the behavioural scorecard, compare two releases, summarise a run.
//!
//! ```sh
//! export FLY_ROM=...            # the cartridge, never copied anywhere
//! fly-scorecard list                                  # the set, resolved, with each rung
//! fly-scorecard suite --label v0.7.5 --out card.json  # the set x seeds x minutes
//! fly-scorecard compare old.json new.json             # exit 1 when a metric regressed
//! fly-scorecard summary card.json                     # a one-screen markdown
//! fly-scorecard run-one --checkpoint X --seed 1       # one run, JSON on stdout
//! ```
//!
//! The checkpoint set is `tools/scorecard/checkpoints.json`: each entry names the environment
//! variable that holds the checkpoint's path (the ROM tests' own, `rom-env.sh`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use flysim::snapshot::MacroMode;

use fly_scorecard::compare::compare;
use fly_scorecard::runner::{Runtime, RunSpec, build_info, run_legacy, run_session};
use fly_scorecard::tally::RunReport;
use fly_scorecard::suite::{
    CheckpointSet, ChildArgs, Job, SCHEMA, SuiteConfig, SuiteReport, note, run_jobs,
};

#[derive(Debug, Parser)]
#[command(name = "fly-scorecard", about = "The behavioural scorecard: multi-seed good-play regression.")]
struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Resolve the checkpoint set and print each checkpoint's rung.
    List {
        #[arg(long, default_value = "tools/scorecard/checkpoints.json")]
        set: PathBuf,
        /// Extra checkpoints, `id=path`.
        #[arg(long = "extra", value_name = "ID=PATH")]
        extra: Vec<String>,
    },
    /// One run: a checkpoint, a seed. Prints the run's JSON on stdout.
    RunOne {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        checkpoint: PathBuf,
        #[arg(long, default_value_t = 1)]
        seed: u32,
        #[arg(long, default_value_t = 10.0)]
        minutes: f64,
        #[arg(long, default_value = "session")]
        runtime: String,
        #[arg(long, default_value = "macros")]
        mode: String,
        #[arg(long, default_value_t = 1)]
        threads: usize,
        #[arg(long, default_value_t = 120.0)]
        probe_s: f64,
        #[arg(long, default_value_t = 600.0)]
        window_s: f64,
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Write the JSON here instead of stdout (the emulator library prints on stdout).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// The set times the seeds, `--jobs` at a time, as one JSON.
    Suite {
        #[arg(long, default_value = "tools/scorecard/checkpoints.json")]
        set: PathBuf,
        #[arg(long = "extra", value_name = "ID=PATH")]
        extra: Vec<String>,
        /// Only these checkpoint ids (comma separated).
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Seeds 1..=K.
        #[arg(long, default_value_t = 3)]
        seeds: u32,
        #[arg(long, default_value_t = 10.0)]
        minutes: f64,
        #[arg(long, default_value = "session")]
        runtime: String,
        #[arg(long, default_value = "macros")]
        mode: String,
        /// Sweep threads per run. One: a run's pool spins at its barriers, so on a box shared with
        /// other jobs one-thread runs lose far less than four-thread ones, and the result does
        /// not depend on the count.
        #[arg(long, default_value_t = 1)]
        threads: usize,
        /// Runs at a time.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
        #[arg(long, default_value_t = 120.0)]
        probe_s: f64,
        #[arg(long, default_value_t = 600.0)]
        window_s: f64,
        /// Names the release or commit under test.
        #[arg(long)]
        label: String,
        #[arg(long)]
        out: PathBuf,
        /// Also write the one-screen summary here.
        #[arg(long)]
        md: Option<PathBuf>,
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Fail instead of running a smaller set when a checkpoint of the set is missing.
        #[arg(long)]
        strict: bool,
        /// Keep the runs an earlier invocation finished (`<out>.runs.jsonl`) and run the rest.
        #[arg(long)]
        resume: bool,
    },
    /// Release A against release B. Exit 1 when a metric regressed.
    Compare {
        a: PathBuf,
        b: PathBuf,
        #[arg(long, default_value_t = 0.05)]
        alpha: f64,
        #[arg(long)]
        json: Option<PathBuf>,
        #[arg(long)]
        md: Option<PathBuf>,
    },
    /// A one-screen markdown of one suite.
    Summary {
        file: PathBuf,
        #[arg(long, default_value = "tools/scorecard/checkpoints.json")]
        set: PathBuf,
    },
}

fn env_dataset(given: Option<PathBuf>) -> PathBuf {
    given
        .or_else(|| std::env::var_os("FLY_MACRO_BRAIN").map(PathBuf::from))
        .or_else(|| std::env::var_os("FLY_DATASET").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("data/fafb-v783"))
}

fn rom() -> Result<PathBuf> {
    std::env::var_os("FLY_ROM").map(PathBuf::from).ok_or_else(|| anyhow!("FLY_ROM is not set"))
}

fn mode_of(text: &str) -> Result<MacroMode> {
    match text {
        "raw" => Ok(MacroMode::Raw),
        "macros" => Ok(MacroMode::Macros),
        other => bail!("mode {other:?} is not raw or macros"),
    }
}

fn parse_extra(extra: &[String]) -> Result<Vec<(String, PathBuf)>> {
    extra
        .iter()
        .map(|item| {
            let (id, path) = item.split_once('=').ok_or_else(|| anyhow!("--extra wants ID=PATH, got {item:?}"))?;
            Ok((id.to_owned(), PathBuf::from(path)))
        })
        .collect()
}

/// The rung a checkpoint stands on, from its reward ledger.
fn rung_of(path: &Path) -> Result<(u32, &'static str, usize)> {
    use flybrain_gb::adapter::GameAdapter;
    let checkpoint = flysim::store::load(path)?;
    let mut adapter = flybrain_gb::pokemon_red::PokemonRedReward::new();
    adapter.import_state(&checkpoint.runtime.reward).map_err(|e| anyhow!("{e}"))?;
    let progress = adapter.progress();
    Ok((progress.rank, progress.rank_label, progress.unique_locations))
}

struct Resolved {
    id: String,
    path: PathBuf,
    rung: Option<u32>,
}

fn resolve(set_path: &Path, extra: &[String], only: &[String], strict: bool) -> Result<Vec<Resolved>> {
    let set = CheckpointSet::load(set_path)?;
    let (found, missing) = set.resolve(&|name| std::env::var(name).ok());
    if !missing.is_empty() {
        eprintln!("scorecard: {} checkpoints of the set are missing: {}", missing.len(), missing.join(", "));
        if strict {
            bail!("--strict: the set is not complete");
        }
    }
    let mut out: Vec<Resolved> = found
        .into_iter()
        .map(|(entry, path)| Resolved { id: entry.id, path, rung: Some(entry.rung) })
        .collect();
    for (id, path) in parse_extra(extra)? {
        if !path.is_file() {
            bail!("--extra {id}: {} is not a file", path.display());
        }
        out.push(Resolved { id, path, rung: None });
    }
    if !only.is_empty() {
        out.retain(|r| only.contains(&r.id));
    }
    if out.is_empty() {
        bail!("no checkpoint to run");
    }
    Ok(out)
}

fn main() {
    if let Err(error) = real_main() {
        eprintln!("fly-scorecard: {error:#}");
        std::process::exit(2);
    }
}

fn real_main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Cmd::List { set, extra } => {
            for r in resolve(&set, &extra, &[], false)? {
                match rung_of(&r.path) {
                    Ok((rank, label, places)) => println!(
                        "{:<28} set rung {:>2}  checkpoint rung {rank:>2} {label} ({places} places)",
                        r.id,
                        r.rung.map_or("-".to_owned(), |n| n.to_string())
                    ),
                    Err(error) => println!("{:<28} unreadable: {error:#}", r.id),
                }
            }
            Ok(())
        }
        Cmd::RunOne { id, checkpoint, seed, minutes, runtime, mode, threads, probe_s, window_s, dataset, out } => {
            let runtime = Runtime::parse(&runtime)?;
            let id = id.unwrap_or_else(|| {
                checkpoint.file_stem().map_or_else(|| "checkpoint".to_owned(), |s| s.to_string_lossy().into_owned())
            });
            let spec = RunSpec {
                id,
                checkpoint,
                rom: rom()?,
                dataset: env_dataset(dataset),
                seed,
                minutes,
                threads,
                mode: mode_of(&mode)?,
                tally: RunSpec::tally_config(probe_s, window_s),
            };
            let report = match runtime {
                Runtime::Legacy => run_legacy(&spec)?,
                Runtime::Session => tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()?
                    .block_on(run_session(&spec))?,
            };
            let text = serde_json::to_string(&report)? + "\n";
            match out {
                Some(path) => std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?,
                None => print!("{text}"),
            }
            Ok(())
        }
        Cmd::Suite {
            set, extra, only, seeds, minutes, runtime, mode, threads, jobs, probe_s, window_s, label, out, md,
            dataset, strict, resume,
        } => {
            Runtime::parse(&runtime)?;
            mode_of(&mode)?;
            rom()?;
            let resolved = resolve(&set, &extra, &only, strict)?;
            let exe = std::env::current_exe().context("finding this executable")?;
            let seed_list: Vec<u32> = (1..=seeds).collect();
            // Every finished run is appended to `<out>.runs.jsonl` at once, so a suite that dies
            // (a box that reboots, a job that is killed) keeps what it finished.
            let runs_log = out.with_extension("runs.jsonl");
            let mut prior: Vec<RunReport> = Vec::new();
            if resume {
                if let Ok(text) = std::fs::read_to_string(&runs_log) {
                    prior = text
                        .lines()
                        .filter_map(|line| serde_json::from_str::<RunReport>(line).ok())
                        .filter(|run| run.runtime == runtime && run.mode == mode && run.minutes >= minutes - 0.01)
                        .collect();
                }
            } else {
                let _ = std::fs::remove_file(&runs_log);
            }
            let mut queue = Vec::new();
            for r in &resolved {
                for seed in &seed_list {
                    if prior.iter().any(|run| run.checkpoint == r.id && run.seed == *seed) {
                        continue;
                    }
                    queue.push(Job { id: r.id.clone(), path: r.path.clone(), seed: *seed });
                }
            }
            if !prior.is_empty() {
                eprintln!("scorecard: resuming with {} finished runs, {} to go", prior.len(), queue.len());
            }
            let child = ChildArgs {
                runtime: runtime.clone(),
                mode: mode.clone(),
                minutes,
                threads,
                probe_s,
                window_s,
                dataset: dataset.clone().or_else(|| Some(env_dataset(None))),
            };
            let progress = out.with_extension("progress.log");
            let _ = std::fs::remove_file(&progress);
            eprintln!(
                "scorecard {label}: {} runs ({} checkpoints x {} seeds) of {minutes} brain min, {jobs} at a time, {threads} threads each",
                queue.len(),
                resolved.len(),
                seed_list.len()
            );
            let began = std::time::Instant::now();
            let outcomes = run_jobs(&exe, queue, &child, jobs, &|outcome| {
                let line = match outcome {
                    Ok(run) => format!(
                        "{:>6.0}s done {} seed {}: rung {}->{} in {:.1} min, {:.0}s wall",
                        began.elapsed().as_secs_f64(), run.checkpoint, run.seed, run.rungs.start, run.rungs.end,
                        run.minutes, run.wall_seconds
                    ),
                    Err(failed) => format!(
                        "{:>6.0}s FAILED {} seed {}: {}",
                        began.elapsed().as_secs_f64(), failed.checkpoint, failed.seed, failed.error
                    ),
                };
                eprintln!("{line}");
                note(&progress, &line);
                if let Ok(run) = outcome
                    && let Ok(text) = serde_json::to_string(run)
                {
                    note(&runs_log, &text);
                }
            });
            let mut runs = prior;
            let mut failed_runs = Vec::new();
            for outcome in outcomes {
                match outcome {
                    Ok(run) => runs.push(run),
                    Err(failed) => failed_runs.push(failed),
                }
            }
            let order: BTreeMap<&str, usize> =
                resolved.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();
            runs.sort_by_key(|run| (order.get(run.checkpoint.as_str()).copied().unwrap_or(usize::MAX), run.seed));
            let suite = SuiteReport {
                schema: SCHEMA.to_owned(),
                label,
                build: build_info(""),
                runtime,
                mode,
                config: SuiteConfig { minutes, seeds: seed_list, threads, jobs, probe_s, window_s },
                runs,
                failed_runs,
            };
            let mut suite = suite;
            suite.build.release = suite.label.clone();
            std::fs::write(&out, serde_json::to_string_pretty(&suite)? + "\n")
                .with_context(|| format!("writing {}", out.display()))?;
            let rungs: BTreeMap<String, u32> =
                resolved.iter().filter_map(|r| r.rung.map(|n| (r.id.clone(), n))).collect();
            let markdown = suite.markdown(&rungs);
            if let Some(md) = md {
                std::fs::write(&md, &markdown).with_context(|| format!("writing {}", md.display()))?;
            }
            println!("{markdown}");
            eprintln!("scorecard: {:.0} s wall", began.elapsed().as_secs_f64());
            if suite.runs.is_empty() {
                bail!("every run failed");
            }
            Ok(())
        }
        Cmd::Compare { a, b, alpha, json, md } => {
            let (sa, sb) = (SuiteReport::load(&a)?, SuiteReport::load(&b)?);
            let comparison = compare(&sa, &sb, alpha);
            let markdown = comparison.markdown();
            if let Some(json) = json {
                std::fs::write(&json, serde_json::to_string_pretty(&comparison)? + "\n")?;
            }
            if let Some(md) = md {
                std::fs::write(&md, &markdown)?;
            }
            println!("{markdown}");
            if !comparison.pass {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Summary { file, set } => {
            let suite = SuiteReport::load(&file)?;
            let rungs: BTreeMap<String, u32> = CheckpointSet::load(&set)
                .map(|s| s.checkpoints.into_iter().map(|e| (e.id, e.rung)).collect())
                .unwrap_or_default();
            println!("{}", suite.markdown(&rungs));
            Ok(())
        }
    }
}
