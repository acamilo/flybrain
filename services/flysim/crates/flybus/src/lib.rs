//! `flybus`: a small local RPC and pub/sub bus with immutable file-backed artifacts.
//!
//! One library, one router, one wire protocol. Small JSON messages carry metadata and
//! artifact references; large immutable bytes live in a file store the router manages, and
//! ownership follows deliveries and explicit holds. The router moves messages and tracks
//! ownership; it knows nothing about what the messages mean.
//!
//! ```text
//!  Client ── Transport (in-memory pipe or Unix socket) ── Router ── State (one mutex)
//!    │                                                      │
//!    └── Artifact / ArtifactWriter ── files ──────────── Store (<root>/<storeId>/...)
//! ```
//!
//! The draft contract this implements is bus-v1 (the session-framework design); the README
//! lists the API, the limits and every place the implementation narrows or extends the draft.

pub mod error;
pub mod limits;
pub mod policy;
pub mod wire;

mod client;
mod router;
mod store;
mod transport;

pub use client::{
    Artifact, ArtifactFile, ArtifactWriter, Artifacts, CancelState, Client, ClientConfig, Message,
    Mode, PendingCall, PublishReceipt, Request, Responder, Retained, RpcResult, Service,
    ServiceConfig, SessionInfo, Subscription, SubscriptionConfig, TopicInfo,
};
pub use error::{BusError, Dispatch, ErrorCode};
pub use limits::Limits;
pub use policy::{Grants, Pattern, Policy};
pub use router::{Router, RouterConfig, RouterStats, UnixListenerHandle};
pub use transport::{Stream, Transport};
pub use wire::{ArtifactRef, Identity};
