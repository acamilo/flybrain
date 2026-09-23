//! `fly-session-types`: the executable schemas of the session framework (CONTRACT-01).
//!
//! What this crate is:
//!
//! - the domain scalars of [ipc-v1] section 2 ([`scalar`]), reusing the bus's `Id`, `U64` and
//!   `Digest` encodings rather than restating them;
//!   the domain request/reply envelope and error codes of sections 3 and 7 ([`rpc`]);
//! - the closed enums and method payloads of [workers-v1] ([`workers`]), the native media
//!   and State.* payloads of [state-media-v1] ([`media`]), and the publication types of
//!   [publishing-v1] ([`publishing`]);
//! - canonical JSON (RFC 8785), canonical digests, the operation key and the canonical body
//!   rules of ipc-v1 section 5 ([`canonical`]);
//! - the documented canonical schema set and `contractDigest` ([`schema`]);
//! - the trace format of [step-v1] section 8, with behaviour separated from operational
//!   metadata and a comparator over behaviour alone ([`trace`]);
//! - the 2026-09-23 extension methods ([`extensions`]) and the legacy Game Boy composition
//!   ([`gameboy`]), whose registered schemas are digested apart from `contractDigest`;
//! - `seed-derivation-v1` ([`seed`]) and the `FLYSESS1` checkpoint envelope layout
//!   ([`checkpoint`]), the two specifications CONTRACT-01 has to settle before the real-agent
//!   and store slices.
//!
//! What it is not: a transport, a worker, a coordinator or a store. It holds no Game Boy FFI
//! and no Melee parser, its generic types hold no console-specific state (the [`gameboy`]
//! declarations travel only inside `TypedValue`s and composition documents), and it never
//! reaches the network.
//!
//! Every type implements [`scalar::DomainType`]: `from_json` reads and validates, `to_json`
//! writes the canonical shape, and `validate` re-checks the rules that span fields. Reading
//! refuses unknown fields, so a payload with a misspelled required field fails instead of
//! silently defaulting.
//!
//! [ipc-v1]: ../../../../docs/design/session-framework/ipc-v1.md
//! [workers-v1]: ../../../../docs/design/session-framework/workers-v1.md
//! [state-media-v1]: ../../../../docs/design/session-framework/state-media-v1.md
//! [publishing-v1]: ../../../../docs/design/session-framework/publishing-v1.md
//! [step-v1]: ../../../../docs/design/session-framework/step-v1.md

pub mod canonical;
pub mod checkpoint;
pub mod extensions;
pub mod fixtures;
pub mod gameboy;
pub mod media;
pub mod publishing;
pub mod rpc;
pub mod scalar;
pub mod schema;
pub mod seed;
pub mod trace;
pub mod workers;

pub use canonical::{OperationKey, body_digest, canonicalize, digest_of};
pub use scalar::{
    ArtifactIdentity, BusCallId, DomainRequestId, DomainType, OwnerKind, OwnerToken, RationalNs,
    SchemaRef, Scope, TypedValue,
};
pub use schema::contract_digest;
pub use trace::{TraceBehaviour, TraceOperational, TransitionTrace};

/// The bus `ArtifactRef` these contracts reference. Re-exported so a consumer does not have
/// to decide whether the domain has its own copy: it does not.
pub use flybus::wire::ArtifactRef;
