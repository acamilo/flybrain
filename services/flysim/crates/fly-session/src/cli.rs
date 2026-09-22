//! The subcommands of this crate's one binary.
//!
//! `implementation.md` section 2 allows worker executables to be subcommands of one binary
//! rather than separate crates, and that is what these are: `agent` and `environment` are the
//! two worker roles a launcher starts as separate processes, and `measure` runs the execution
//! modes against each other.
//!
//! ```text
//! fly-session agent       --socket S --store-root D --client-id C --service N --threads T ...
//! fly-session environment --socket S --store-root D --client-id C --service N --threads T ...
//! fly-session measure     [--steps N] [--agents 1,2,4] [--modes in-process,thread,process]
//! ```
//!
//! A worker process is told exactly which participant it is. It proves that identity in
//! `Worker.Hello`, so a process started under another one is refused by its own supervisor
//! before the coordinator has pinned anything.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::agent::AgentFaults;
use crate::environment::EnvironmentFaults;
use crate::launcher::{
    AgentLaunch, EnvironmentLaunch, ExecutionMode, Started, flags, serve_one,
};
use crate::types::*;

const USAGE: &str = "\
fly-session <command> [options]

  agent          serve one agent worker on a launcher-created endpoint
  environment    serve the environment worker on a launcher-created endpoint
  measure        compare the execution modes and print the measurement table
  measure-row    measure one row and print it as JSON (one child per row)

Worker options (agent and environment):
  --socket PATH        the launcher's endpoint for this participant
  --store-root PATH    the router's artifact store root
  --client-id ID       the configured bus client identity
  --service NAME       the one service name this worker registers
  --threads N          the launcher's thread allocation for this worker
  --session ID         the session this worker belongs to
  --incarnation ID     this worker's domain incarnation

  agent:        --agent ID --port ID --tick-numerator N --tick-denominator N
                --warmup-ticks N [--prepare-delay-ms N] [--commit-delay-ms N]
                [--fail-commit-at-step N]
  environment:  --worker ID --ports p1,p2 --step-numerator N --step-denominator N
                [--advance-delay-ms N] [--omit-view-at-boundary N]

Measure options:
  --steps N            transitions per run (default 200)
  --warmup-steps N     transitions run before sampling starts (default 10)
  --agents 1,2,4       agent counts to compare (default 1,2,4)
  --modes LIST         in-process, thread, process (default all three)
  --worker-threads N   within-agent worker threads (default 1)

measure-row options: --mode NAME --agents N, plus the measure options above. Each row runs in
a process of its own, so its memory peak is its own rather than the peak of the rows before
it.
";

/// The binary's entry point.
pub fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let command = command.to_string_lossy().into_owned();
    let rest: Vec<String> = args.map(|a| a.to_string_lossy().into_owned()).collect();
    let result = match command.as_str() {
        "agent" => Options::parse(&rest, &[flags::COMMON, flags::AGENT_ONLY])
            .and_then(|o| serve(&command, &o)),
        "environment" => Options::parse(&rest, &[flags::COMMON, flags::ENVIRONMENT_ONLY])
            .and_then(|o| serve(&command, &o)),
        "measure" => Options::parse(&rest, &[flags::MEASURE]).and_then(|o| measure(&o)),
        "measure-row" => Options::parse(&rest, &[flags::MEASURE]).and_then(|o| measure_row(&o)),
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fly-session {command}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--flag value` options. The launcher builds this argv, so the grammar stays small.
#[derive(Debug, Default)]
struct Options(BTreeMap<String, String>);

impl Options {
    /// Reads the options of one command, refusing any flag that command does not have.
    ///
    /// `allowed` comes from [`crate::launcher::flags`], the same constants the launcher
    /// writes the argv from. An unknown flag is an error naming it rather than a value that
    /// is quietly ignored: a renamed option must fail the launch, not turn into a no-op that
    /// no test notices.
    fn parse(args: &[String], allowed: &[&[&str]]) -> Result<Options, String> {
        let mut out = BTreeMap::new();
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let Some(name) = flag.strip_prefix("--") else {
                return Err(format!("expected an option, found {flag:?}"));
            };
            if !allowed.iter().any(|set| set.contains(&name)) {
                return Err(format!("unknown option --{name} for this command"));
            }
            let value = iter
                .next()
                .ok_or_else(|| format!("option --{name} needs a value"))?;
            if out.insert(name.to_owned(), value.clone()).is_some() {
                return Err(format!("option --{name} was given twice"));
            }
        }
        Ok(Options(out))
    }

    fn required(&self, name: &str) -> Result<&str, String> {
        self.0
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("option --{name} is required"))
    }

    fn optional(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    fn id(&self, name: &str) -> Result<Id, String> {
        parse_id(self.required(name)?).map_err(|e| format!("--{name}: {e}"))
    }

    fn u64(&self, name: &str, default: u64) -> Result<u64, String> {
        match self.0.get(name) {
            None => Ok(default),
            Some(value) => value.parse().map_err(|_| format!("--{name}: {value:?} is not a number")),
        }
    }

    fn opt_u64(&self, name: &str) -> Result<Option<u64>, String> {
        match self.0.get(name) {
            None => Ok(None),
            Some(value) => value
                .parse()
                .map(Some)
                .map_err(|_| format!("--{name}: {value:?} is not a number")),
        }
    }

    fn usize(&self, name: &str, default: usize) -> Result<usize, String> {
        Ok(self.u64(name, default as u64)? as usize)
    }

    fn path(&self, name: &str) -> Result<PathBuf, String> {
        Ok(PathBuf::from(self.required(name)?))
    }

    fn rational(&self, numerator: &str, denominator: &str) -> Result<RationalNs, String> {
        let n = self.u64(numerator, 0)?;
        let d = self.u64(denominator, 1)?;
        RationalNs::new(n, d).map_err(|e| format!("--{numerator}/--{denominator}: {}", e.0))
    }
}

/// Serves one worker until `Worker.Shutdown`, then exits.
fn serve(role: &str, options: &Options) -> Result<(), String> {
    let socket = options.path(flags::SOCKET)?;
    let store_root = options.path(flags::STORE_ROOT)?;
    let client_id = options.required(flags::CLIENT_ID)?.to_owned();
    let service = options.required(flags::SERVICE)?.to_owned();
    let threads = options.usize(flags::THREADS, 1)?;
    if threads == 0 {
        return Err("--threads must be at least 1".to_owned());
    }
    let session_id = options.id(flags::SESSION)?;
    let incarnation_id = options.id(flags::INCARNATION)?;
    let what = match role {
        "agent" => Started::Agent(AgentLaunch {
            session_id,
            agent_id: options.id(flags::AGENT)?,
            port_id: options.id(flags::PORT)?,
            incarnation_id,
            tick_duration: options.rational(flags::TICK_NUMERATOR, flags::TICK_DENOMINATOR)?,
            warmup_ticks: options.u64(flags::WARMUP_TICKS, 0)?,
            worker_threads: threads,
            graph_variant: options.u64(flags::GRAPH_VARIANT, 0)?,
            // This process's own log. The supervisor reads what crosses the bus, not this.
            sensors: crate::media::SensorLog::new(),
            faults: AgentFaults {
                fail_commit_at_step: options.opt_u64(flags::FAIL_COMMIT_AT_STEP)?,
                prepare_delay_ms: options.u64(flags::PREPARE_DELAY_MS, 0)?,
                commit_delay_ms: options.u64(flags::COMMIT_DELAY_MS, 0)?,
            },
            client_id: client_id.clone(),
            service: service.clone(),
        }),
        _ => Started::Environment(EnvironmentLaunch {
            session_id,
            worker_id: options.id(flags::WORKER)?,
            incarnation_id,
            step_duration: options.rational(flags::STEP_NUMERATOR, flags::STEP_DENOMINATOR)?,
            ports: parse_ports(options.required(flags::PORTS)?)?,
            worker_threads: threads,
            observation_delay_steps: options.u64(flags::OBSERVATION_DELAY_STEPS, 0)?,
            renders: crate::media::RenderCounter::new(),
            faults: EnvironmentFaults {
                advance_delay_ms: options.u64(flags::ADVANCE_DELAY_MS, 0)?,
                omit_view_at_boundary: options.opt_u64(flags::OMIT_VIEW_AT_BOUNDARY)?,
                stale_view_at_boundary: options.opt_u64(flags::STALE_VIEW_AT_BOUNDARY)?,
                truncated_view_at_boundary: options
                    .opt_u64(flags::TRUNCATED_VIEW_AT_BOUNDARY)?,
                omit_audio_at_boundary: options.opt_u64(flags::OMIT_AUDIO_AT_BOUNDARY)?,
                overlapping_audio_at_boundary: options
                    .opt_u64(flags::OVERLAPPING_AUDIO_AT_BOUNDARY)?,
            },
            client_id: client_id.clone(),
            service: service.clone(),
        }),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    runtime.block_on(async move {
        let handle = serve_one(&socket, &client_id, &service, &store_root, &what, threads).await?;
        // The worker serves until its supervisor's Worker.Shutdown, which it answers before
        // it stops. Exiting is then one event, not a race between a reply and a signal.
        handle.join().await;
        Ok(())
    })
}

fn parse_ports(value: &str) -> Result<Vec<Id>, String> {
    value
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| parse_id(part).map_err(|e| format!("--ports: {e}")))
        .collect()
}

fn measure_config(options: &Options) -> Result<crate::measure::MeasureConfig, String> {
    let mut config = crate::measure::MeasureConfig {
        steps: options.u64("steps", 200)?,
        warmup_steps: options.u64("warmup-steps", 10)?,
        worker_threads: options.usize("worker-threads", 1)?,
        ..crate::measure::MeasureConfig::default()
    };
    if let Some(list) = options.optional("agents") {
        config.agent_counts = list
            .split(',')
            .filter(|p| !p.is_empty())
            .map(|p| p.parse::<usize>().map_err(|_| format!("--agents: {p:?}")))
            .collect::<Result<Vec<usize>, String>>()?;
    }
    if let Some(list) = options.optional("modes") {
        config.modes = list
            .split(',')
            .filter(|p| !p.is_empty())
            .map(parse_mode)
            .collect::<Result<Vec<ExecutionMode>, String>>()?;
    }
    Ok(config)
}

fn parse_mode(name: &str) -> Result<ExecutionMode, String> {
    match name {
        "in-process" => Ok(ExecutionMode::InProcess),
        "thread" => Ok(ExecutionMode::Thread),
        "process" => Ok(ExecutionMode::Process),
        other => Err(format!("unknown mode {other:?}")),
    }
}

/// Runs the execution-mode comparison and prints its table.
///
/// One child per row: a peak-memory figure is only that row's if nothing else ran in the
/// process that produced it.
fn measure(options: &Options) -> Result<(), String> {
    let config = measure_config(options)?;
    let program = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    let rows = crate::measure::run(&config, &program)?;
    print!("{}", crate::measure::table(&rows));
    Ok(())
}

/// Measures exactly one row and prints it as one JSON object. The parent's child.
fn measure_row(options: &Options) -> Result<(), String> {
    let config = measure_config(options)?;
    let mode = parse_mode(options.required("mode")?)?;
    let agents = options.usize("agents", 2)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let row = runtime.block_on(crate::measure::one(&config, mode, agents))?;
    println!("{}", row.to_json());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unknown flag is refused by name. A renamed option must fail the launch rather than
    /// be accepted and ignored, which would turn a fault or a delay into a no-op.
    #[test]
    fn an_unknown_option_is_refused_by_name() {
        let args: Vec<String> = ["--session", "demo", "--stale-view-at-boundry", "2"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let error = Options::parse(&args, &[flags::COMMON, flags::ENVIRONMENT_ONLY])
            .expect_err("an unknown option is refused");
        assert!(error.contains("--stale-view-at-boundry"), "{error}");
    }

    /// A flag that belongs to another command is refused too: an agent has no render delay.
    #[test]
    fn an_option_of_another_command_is_refused() {
        let args: Vec<String> = ["--observation-delay-steps", "2"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        Options::parse(&args, &[flags::COMMON, flags::AGENT_ONLY])
            .expect_err("the agent command has no render delay");
        Options::parse(&args, &[flags::COMMON, flags::ENVIRONMENT_ONLY])
            .expect("the environment command does");
    }
}
