//! Shared harness: a router on a temporary store, reached over either transport, plus a raw
//! protocol client for adversarial frames the SDK would never send.

#![allow(dead_code)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use flybus::wire::{Envelope, Kind, Location, read_frame};
use flybus::{
    Artifact, Client, ClientConfig, Limits, Policy, Router, RouterConfig, RouterStats, Transport,
    UnixListenerHandle,
};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};

pub const WAIT: Duration = Duration::from_secs(10);

/// Generates one test per transport from an `async fn name(via: Via)`.
#[macro_export]
macro_rules! both_transports {
    ($($name:ident),* $(,)?) => {
        mod in_memory {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
                async fn $name() {
                    super::$name($crate::common::Via::Memory).await
                }
            )*
        }
        mod unix_socket {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
                async fn $name() {
                    super::$name($crate::common::Via::Unix).await
                }
            )*
        }
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Memory,
    Unix,
}

pub struct Env {
    pub router: Router,
    pub via: Via,
    pub dir: tempfile::TempDir,
    listeners: Mutex<Vec<UnixListenerHandle>>,
    next_socket: AtomicU64,
}

pub async fn env(via: Via) -> Env {
    env_with(via, Limits::default(), Policy::open()).await
}

pub async fn env_with(via: Via, limits: Limits, policy: Policy) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let mut config = RouterConfig::new(dir.path().join("store"));
    config.limits = limits;
    config.policy = policy;
    let router = Router::new(config).unwrap();
    Env {
        router,
        via,
        dir,
        listeners: Mutex::new(Vec::new()),
        next_socket: AtomicU64::new(0),
    }
}

impl Env {
    pub async fn transport(&self) -> Transport {
        match self.via {
            Via::Memory => self.router.connect_in_memory(),
            Via::Unix => {
                let n = self.next_socket.fetch_add(1, Ordering::Relaxed);
                let path = self.dir.path().join(format!("unbound-{n}.sock"));
                let listener = self.router.listen_unix(&path).await.unwrap();
                let transport = Transport::unix(&path).await.unwrap();
                self.listeners.lock().unwrap().push(listener);
                transport
            }
        }
    }

    pub async fn transport_as(&self, id: &str) -> Transport {
        self.try_transport_as(id).await.unwrap()
    }

    async fn try_transport_as(&self, id: &str) -> std::io::Result<Transport> {
        match self.via {
            Via::Memory => Ok(self.router.connect_in_memory_as(id)),
            Via::Unix => {
                let n = self.next_socket.fetch_add(1, Ordering::Relaxed);
                let path = self.dir.path().join(format!("bound-{n}.sock"));
                let listener = self.router.listen_unix_as(&path, id).await?;
                let transport = Transport::unix(&path).await?;
                self.listeners.lock().unwrap().push(listener);
                Ok(transport)
            }
        }
    }

    pub fn config(&self, id: &str) -> ClientConfig {
        ClientConfig::new(id, self.router.store_root())
    }

    pub async fn try_client(&self, id: &str) -> Result<Client, flybus::BusError> {
        let transport = self.try_transport_as(id).await.map_err(|e| {
            flybus::BusError::new(flybus::ErrorCode::RouterLost, format!("connect: {e}"))
        })?;
        Client::connect(transport, self.config(id)).await
    }

    pub async fn client(&self, id: &str) -> Client {
        self.try_client(id).await.unwrap()
    }

    pub async fn raw(&self) -> Raw {
        Raw::over(self.transport().await, self.router.store_root())
    }

    pub async fn raw_as(&self, id: &str) -> Raw {
        Raw::over(self.transport_as(id).await, self.router.store_root())
    }

    pub async fn raw_hello(&self, id: &str) -> Raw {
        let mut raw = self.raw_as(id).await;
        raw.hello(id).await.unwrap();
        raw
    }

    pub fn stats(&self) -> RouterStats {
        self.router.stats()
    }

    /// Polls the router until `ok` holds; panics with the last stats after [`WAIT`].
    pub async fn settle(&self, what: &str, ok: impl Fn(&RouterStats) -> bool) -> RouterStats {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let s = self.stats();
            if ok(&s) {
                return s;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("{what}: router never settled: {s:?}");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Files currently in the store's `sealed/` or `staging/` directory.
    pub fn files(&self, sub: &str) -> usize {
        std::fs::read_dir(self.router.store_dir().join(sub))
            .map(|d| d.count())
            .unwrap_or(0)
    }

    /// Waits for the file count to reach `n`. Unlinks follow the registry update, outside the
    /// router lock, so a file can briefly outlive its entry.
    pub async fn settle_files(&self, sub: &str, n: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.files(sub) != n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{sub}: {} files, wanted {n}",
                self.files(sub)
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

pub fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

pub async fn within<T>(what: &str, f: impl Future<Output = T>) -> T {
    match tokio::time::timeout(WAIT, f).await {
        Ok(v) => v,
        Err(_) => panic!("{what}: timed out"),
    }
}

/// Asserts nothing arrives for a short while.
pub async fn quiet<T: std::fmt::Debug>(what: &str, f: impl Future<Output = Option<T>>) {
    if let Ok(Some(v)) = tokio::time::timeout(Duration::from_millis(150), f).await {
        panic!("{what}: unexpected {v:?}");
    }
}

pub async fn sealed(client: &Client, bytes: &[u8], content_type: &str) -> Artifact {
    use std::io::Write;
    let mut w = client
        .artifacts()
        .allocate(bytes.len() as u64, content_type)
        .await
        .unwrap();
    w.write_all(bytes).unwrap();
    w.seal().await.unwrap()
}

/// The write half of a [`Raw`], for a task that floods the router.
pub struct RawWriter {
    wr: WriteHalf<Transport>,
}

impl RawWriter {
    /// Sends one frame; false once the router has gone.
    pub async fn send(&mut self, bytes: &[u8]) -> bool {
        let mut buf = (bytes.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(bytes);
        self.wr.write_all(&buf).await.is_ok()
    }
}

/// A client speaking the wire protocol by hand.
pub struct Raw {
    rd: ReadHalf<Transport>,
    wr: Option<WriteHalf<Transport>>,
    pub next: u64,
    pub stash: Vec<Envelope>,
    store_root: PathBuf,
}

pub type Reply = Result<Map<String, Value>, (String, String)>;

impl Raw {
    pub fn over(transport: Transport, store_root: &Path) -> Raw {
        let (rd, wr) = tokio::io::split(transport);
        Raw {
            rd,
            wr: Some(wr),
            next: 0,
            stash: Vec::new(),
            store_root: store_root.to_path_buf(),
        }
    }

    pub fn take_writer(&mut self) -> RawWriter {
        RawWriter {
            wr: self.wr.take().expect("writer already taken"),
        }
    }

    fn wr(&mut self) -> &mut WriteHalf<Transport> {
        self.wr.as_mut().expect("writer was taken")
    }

    pub async fn send_bytes(&mut self, bytes: &[u8]) {
        let mut buf = (bytes.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(bytes);
        let _ = self.wr().write_all(&buf).await;
        let _ = self.wr().flush().await;
    }

    pub async fn send_prefix(&mut self, len: u32) {
        let _ = self.wr().write_all(&len.to_le_bytes()).await;
        let _ = self.wr().flush().await;
    }

    /// Sends a command with the next id; returns the id.
    pub async fn command(&mut self, op: &str, body: Value, attachments: Value) -> String {
        self.next += 1;
        let id = format!("msg-{}", self.next);
        let env = json!({
            "protocol": "flybus", "major": 1, "minor": 0,
            "id": id, "replyTo": null, "kind": "command", "op": op,
            "body": body, "attachments": attachments,
        });
        self.send_bytes(&serde_json::to_vec(&env).unwrap()).await;
        id
    }

    /// The next envelope, `None` at end of stream.
    pub async fn recv(&mut self) -> Option<Envelope> {
        if !self.stash.is_empty() {
            return Some(self.stash.remove(0));
        }
        self.read().await
    }

    async fn read(&mut self) -> Option<Envelope> {
        match within("raw read", read_frame(&mut self.rd)).await {
            Ok(Some(bytes)) => Some(Envelope::decode(&bytes).expect("router frames are valid")),
            _ => None,
        }
    }

    /// Waits for the reply to `id`, stashing anything else.
    pub async fn reply(&mut self, id: &str) -> Reply {
        if let Some(i) = self
            .stash
            .iter()
            .position(|e| e.reply_to.as_deref() == Some(id))
        {
            return parse_reply(self.stash.remove(i));
        }
        loop {
            let env = self
                .read()
                .await
                .unwrap_or_else(|| panic!("closed while waiting for {id}"));
            if env.reply_to.as_deref() == Some(id) {
                return parse_reply(env);
            }
            self.stash.push(env);
        }
    }

    pub async fn call(&mut self, op: &str, body: Value) -> Reply {
        let id = self.command(op, body, json!([])).await;
        self.reply(&id).await
    }

    pub async fn call_with(&mut self, op: &str, body: Value, attachments: Value) -> Reply {
        let id = self.command(op, body, attachments).await;
        self.reply(&id).await
    }

    pub async fn hello(&mut self, id: &str) -> Reply {
        self.call(
            "bus.hello",
            json!({"clientId": id, "clientIncarnation": "inc-raw", "supportedMajors": [1]}),
        )
        .await
    }

    /// Reads to end of stream and returns the last `connection.closing` notice, if any.
    pub async fn closing(&mut self) -> Option<Map<String, Value>> {
        let mut last = None;
        while let Some(env) = self.recv().await {
            if env.kind == Kind::Notice && env.op == "connection.closing" {
                last = Some(env.body);
            }
        }
        last
    }

    /// The next delivery or notice, from the stash first.
    pub async fn event(&mut self) -> Envelope {
        if let Some(i) = self.stash.iter().position(|e| e.kind != Kind::Reply) {
            return self.stash.remove(i);
        }
        loop {
            let env = self
                .read()
                .await
                .expect("closed while waiting for an event");
            if env.kind != Kind::Reply {
                return env;
            }
            self.stash.push(env);
        }
    }

    pub fn path(&self, loc: &Value) -> PathBuf {
        let loc = Location::from_json(loc).unwrap();
        self.store_root.join(loc.store_id).join(loc.relative_path)
    }

    /// Allocates and writes an artifact by hand; returns (ref-less allocate value, staging path).
    pub async fn allocate(&mut self, len: u64) -> (Map<String, Value>, PathBuf) {
        let v = self
            .call(
                "artifact.allocate",
                json!({"byteLength": len.to_string(), "contentType": "application/octet-stream"}),
            )
            .await
            .unwrap();
        let path = self.path(&v["writeLocation"]);
        (v, path)
    }

    pub async fn seal(&mut self, alloc: &Map<String, Value>, digest: Value) -> Reply {
        self.call(
            "artifact.seal",
            json!({"artifactId": alloc["artifactId"], "generation": "1", "ownerId": alloc["ownerId"], "digest": digest}),
        )
        .await
    }
}

pub fn parse_reply(env: Envelope) -> Reply {
    assert_eq!(env.kind, Kind::Reply);
    if env.body["ok"] == json!(true) {
        Ok(env.body["value"].as_object().unwrap().clone())
    } else {
        let e = &env.body["error"];
        Err((
            e["code"].as_str().unwrap().to_owned(),
            e["dispatch"].as_str().unwrap().to_owned(),
        ))
    }
}

pub fn code(r: &Reply) -> &str {
    match r {
        Ok(_) => "OK",
        Err((c, _)) => c,
    }
}

pub fn store_path(root: &Path, store_id: &str, rel: &str) -> PathBuf {
    root.join(store_id).join(rel)
}
