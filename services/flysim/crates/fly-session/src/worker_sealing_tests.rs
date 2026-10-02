//! A bus worker's reply artifacts are sealed by the reply that carries them (BUS-02). These
//! tests hold a worker between recording a reply and sending it, which no client can arrange
//! from outside, so they live beside the shell.
//!
//! - Review N2: a duplicate replayed in that window must never attach the still-unsealed
//!   writer. It waits for the sealing reply and replays the sealed artifact.
//! - Review N1: a store failure while the reply seals reaches the caller as the typed domain
//!   failure the handler's own seal gave before BUS-02 (`BACKEND_FAILURE`, mutation applied),
//!   not as a bus-level failed call, and a retry replays nothing it could not deliver.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Map;
use tokio::sync::Semaphore;

use super::*;
use crate::rpc::{self, WorkerRef};

/// Workers held before their reply until the test lets them go, by worker id.
static GATES: Mutex<Vec<(String, Arc<Semaphore>)>> = Mutex::new(Vec::new());

/// Waits, in a worker under test, until the test opens its gate. Other workers pass.
pub(super) async fn gate(worker: &Id) {
    let gate = GATES
        .lock()
        .unwrap()
        .iter()
        .find(|(w, _)| w == worker.as_str())
        .map(|(_, g)| g.clone());
    if let Some(gate) = gate {
        let _ = gate.acquire().await.expect("never closed");
    }
}

fn hold(worker: &str) -> Arc<Semaphore> {
    let gate = Arc::new(Semaphore::new(0));
    GATES.lock().unwrap().push((worker.to_owned(), gate.clone()));
    gate
}

/// A worker whose `State.Capture` seals `len` bytes of a value that changes every execution.
struct Sealer {
    worker: Id,
    status: StatusCell,
    len: usize,
    executions: Arc<std::sync::atomic::AtomicU64>,
}

impl WorkerEndpoint for Sealer {
    fn worker_id(&self) -> Id {
        self.worker.clone()
    }
    fn incarnation_id(&self) -> Id {
        id("inc-1")
    }
    fn session_id(&self) -> Id {
        id("sealing")
    }
    fn role(&self) -> Role {
        Role::Environment
    }
    fn capabilities(&self) -> Vec<Id> {
        Vec::new()
    }
    fn status_cell(&self) -> StatusCell {
        self.status.clone()
    }
    fn worker_threads(&self) -> u64 {
        1
    }
    fn methods(&self) -> Vec<&'static str> {
        vec!["State.Capture"]
    }
    fn handle<'a>(&'a mut self, ctx: HandlerCtx<'a>) -> BoxFuture<'a, DomainResult<HandlerReply>> {
        Box::pin(async move {
            let n = self.executions.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as u8;
            let blob = ctx.seal("application/octet-stream", vec![0x40 + n; self.len]).await?;
            Ok(HandlerReply::with_artifacts(Map::new(), vec![("blob".into(), blob)]))
        })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    _router: flybus::Router,
    caller: flybus::Client,
    target: WorkerRef,
    executions: Arc<std::sync::atomic::AtomicU64>,
    handle: WorkerHandle,
}

async fn rig(worker: &str, len: usize, max_store_bytes: Option<u64>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut config = flybus::RouterConfig::new(dir.path().join("store"));
    config.policy = flybus::Policy::open();
    if let Some(max) = max_store_bytes {
        config.limits.max_store_bytes = max;
        config.limits.max_artifact_bytes = max;
        config.limits.max_retained_bytes = max;
    }
    let router = flybus::Router::new(config).unwrap();
    let connect = |who: &str| {
        let transport = router.connect_in_memory_as(who);
        flybus::Client::connect(transport, flybus::ClientConfig::new(who, router.store_root()))
    };
    let worker_client = connect(worker).await.unwrap();
    let caller = connect("caller").await.unwrap();
    let service = worker_client
        .register(&format!("w.{worker}"), flybus::ServiceConfig::default())
        .await
        .unwrap();
    let target = WorkerRef::new(service.name(), service.incarnation(), &id(worker));
    let executions = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let endpoint = Sealer {
        worker: id(worker),
        status: StatusCell::new(),
        len,
        executions: executions.clone(),
    };
    let handle = serve(worker_client, service, endpoint);
    Rig { _dir: dir, _router: router, caller, target, executions, handle }
}

async fn capture(rig: &Rig, serial: u64) -> rpc::SentCall {
    rpc::send(
        &rig.caller,
        &rig.target,
        "State.Capture",
        None,
        Map::new(),
        &[],
        DomainRequestId::from_serial(serial),
        &["blob".to_owned()],
    )
    .await
    .unwrap()
}

async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), f)
        .await
        .unwrap_or_else(|_| panic!("{what} timed out"))
}

/// N2: the original's reply is recorded and held unsent; a duplicate arrives, finds the record,
/// and must not attach the writer the held reply is about to seal. Both calls get the same
/// sealed bytes, from one execution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replay_waits_for_the_reply_that_seals_its_artifacts() {
    const LEN: usize = 300_000;
    let rig = rig("n2-worker", LEN, None).await;
    let gate = hold("n2-worker");
    let original = capture(&rig, 1).await;
    // The handler ran and the reply is recorded: the worker is at the gate.
    within("the original executes", async {
        while rig.executions.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let duplicate = capture(&rig, 1).await;
    let duplicate = tokio::spawn(duplicate.finish());
    // Long enough for the duplicate to reach admission and, without the fix, to replay the
    // unsealed handle and race the held reply.
    tokio::time::sleep(Duration::from_millis(300)).await;
    gate.add_permits(1);
    let original = within("original", original.finish()).await.expect("the original succeeds");
    let duplicate = within("duplicate", duplicate)
        .await
        .unwrap()
        .expect("the duplicate replays the same reply");
    assert_eq!(rig.executions.load(std::sync::atomic::Ordering::SeqCst), 1);
    let a = original.artifacts["blob"].read_all().await.unwrap();
    let b = duplicate.artifacts["blob"].read_all().await.unwrap();
    assert_eq!(a, vec![0x40; LEN]);
    assert_eq!(a, b);
    rig.handle.stop().await;
}

/// N1: the store refuses the sealing copy (quota). The caller gets the typed failure, with the
/// mutation applied, as when the handler's own `artifact.seal` was refused before BUS-02; a
/// retry runs again rather than replaying a reply that never reached anyone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_seal_is_a_typed_domain_failure() {
    const LEN: usize = 8192;
    // The writer's allocation fits; its sealed copy does not.
    let rig = rig("n1-worker", LEN, Some(LEN as u64 + 100)).await;
    let reply = within("capture", capture(&rig, 1).await.finish()).await;
    let error = match reply {
        Ok(reply) => reply.result().expect_err("the seal was refused"),
        Err(e) => panic!("a bus-level failure instead of the domain failure: {e:?}"),
    };
    assert_eq!(error.code, ErrorCode::BackendFailure, "{error:?}");
    assert_eq!(error.mutation, MutationCertainty::Applied);
    assert!(error.message.starts_with("seal: "), "{}", error.message);
    assert_eq!(rig.handle.state(), WorkerState::Failed);
    // Nothing of the refused reply is cached: the retry executes again.
    let retry = within("retry", capture(&rig, 1).await.finish()).await.unwrap();
    assert!(retry.result().is_err());
    assert_eq!(rig.executions.load(std::sync::atomic::Ordering::SeqCst), 2);
    rig.handle.stop().await;
}
