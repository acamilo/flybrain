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
use crate::launcher::{AgentLaunch, EnvironmentLaunch, ExecutionMode, Started, serve_one};
use crate::types::*;

const USAGE: &str = "\
fly-session <command> [options]

  agent          serve one agent worker on a launcher-created endpoint
  environment    serve the environment worker on a launcher-created endpoint
  measure        compare the execution modes and print the measurement table

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
  --agents 1,2,4       agent counts to compare (default 1,2,4)
  --modes LIST         in-process, thread, process (default all three)
  --worker-threads N   within-agent worker threads (default 1)
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
        "agent" | "environment" => Options::parse(&rest).and_then(|o| serve(&command, &o)),
        "measure" => Options::parse(&rest).and_then(|o| measure(&o)),
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
    fn parse(args: &[String]) -> Result<Options, String> {
        let mut out = BTreeMap::new();
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let Some(name) = flag.strip_prefix("--") else {
                return Err(format!("expected an option, found {flag:?}"));
            };
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
    let socket = options.path("socket")?;
    let store_root = options.path("store-root")?;
    let client_id = options.required("client-id")?.to_owned();
    let service = options.required("service")?.to_owned();
    let threads = options.usize("threads", 1)?;
    if threads == 0 {
        return Err("--threads must be at least 1".to_owned());
    }
    let session_id = options.id("session")?;
    let incarnation_id = options.id("incarnation")?;
    let what = match role {
        "agent" => Started::Agent(AgentLaunch {
            session_id,
            agent_id: options.id("agent")?,
            port_id: options.id("port")?,
            incarnation_id,
            tick_duration: options.rational("tick-numerator", "tick-denominator")?,
            warmup_ticks: options.u64("warmup-ticks", 0)?,
            worker_threads: threads,
            faults: AgentFaults {
                fail_commit_at_step: options.opt_u64("fail-commit-at-step")?,
                prepare_delay_ms: options.u64("prepare-delay-ms", 0)?,
                commit_delay_ms: options.u64("commit-delay-ms", 0)?,
            },
            client_id: client_id.clone(),
            service: service.clone(),
        }),
        _ => Started::Environment(EnvironmentLaunch {
            session_id,
            worker_id: options.id("worker")?,
            incarnation_id,
            step_duration: options.rational("step-numerator", "step-denominator")?,
            ports: parse_ports(options.required("ports")?)?,
            worker_threads: threads,
            faults: EnvironmentFaults {
                advance_delay_ms: options.u64("advance-delay-ms", 0)?,
                omit_view_at_boundary: options.opt_u64("omit-view-at-boundary")?,
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

/// Runs the execution-mode comparison and prints its table.
fn measure(options: &Options) -> Result<(), String> {
    let mut config = crate::measure::MeasureConfig {
        steps: options.u64("steps", 200)?,
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
            .map(|p| match p {
                "in-process" => Ok(ExecutionMode::InProcess),
                "thread" => Ok(ExecutionMode::Thread),
                "process" => Ok(ExecutionMode::Process),
                other => Err(format!("--modes: {other:?}")),
            })
            .collect::<Result<Vec<ExecutionMode>, String>>()?;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let rows = runtime.block_on(crate::measure::run(&config))?;
    print!("{}", crate::measure::table(&rows));
    Ok(())
}
