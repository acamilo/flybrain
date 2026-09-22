# Flybus v1 conformance report: the `flybus` crate against `bus-v1`

Status: **audit, 2026-09-22**. Subject: `services/flysim/crates/flybus`, the replayed
implementation of [Flybus v1](bus-v1.md) (draft 1, 2026-09-18), with no consumers yet. Scalar
encodings come from [ipc-v1](ipc-v1.md) sections 1 to 3. Acceptance lists come from the
[implementation guide](implementation.md) slices BUS-01, BUS-02 and BUS-03.

Every normative sentence of bus-v1 sections 2 to 11 gets a row. Four statuses:

| Status | Meaning |
| --- | --- |
| conforms | The implementation does what the sentence requires, and a test proves it. |
| deviates-allowed | It differs, and a quoted sentence of the draft permits the difference. |
| deviates-must-fix | It differs and the draft requires otherwise. |
| not-implemented | Not built yet; the row names who owns it. |

Counts over 195 rows: **conforms 178, deviates-allowed 9, deviates-must-fix 1 (fixed),
not-implemented 7**. The audit found the one deviates-must-fix — connection teardown could be
starved for the length of a whole frame by the writer it was waiting for — and it is fixed on
this branch, so the row for it now reads conforms and records the fix (section 9, "cannot be
starved"). Two contradictions inside the draft are recorded at the end and left alone.

Test names below are the functions in `services/flysim/crates/flybus/tests`, 239 of them in
this branch (`cargo test -p flybus`), plus the ignored measurement. Everything marked
"(both)" is generated twice by the `both_transports!` macro, once over the in-memory transport
and once over a Unix socket, so `tests/rpc.rs::request_reply_roundtrip` means
`in_memory::request_reply_roundtrip` and `unix_socket::request_reply_roundtrip`.

## 2. Client API

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| One client/connection serves RPC, pub/sub and artifacts | conforms | `client/mod.rs::Client` | `tests/integration.rs::session_over_one_router` (both) |
| The illustrative surface (`connect`, `register`, `call`, `subscribe`, `publish`, `artifacts().allocate`, `seal`, `message.artifact`) | deviates-allowed: "Illustrative Rust surface (not yet implemented)"; `connect` takes the transport, `call` takes no `budget`, `next()` yields `Option` | `client/mod.rs`, `client/handles.rs` | `tests/rpc.rs`, `tests/pubsub.rs`, `tests/artifacts.rs` |
| The artifact store is a storage backend of the bus, not another messaging service | conforms | `store.rs`, reached only through `Artifacts`/`Artifact` | `tests/artifacts.rs::allocate_write_seal_read` (both) |
| Bulk data does not pass through router socket payloads; no separate data-transfer API | conforms: attachments carry `ArtifactRef` only; envelopes are capped at 64 KiB | `wire.rs::ArtifactRef`, `wire.rs::MAX_ENVELOPE_BYTES` | `tests/wire.rs::envelope_size_limits` (both), `tests/perf.rs` |
| `Artifact` is a read-only, cloneable handle | conforms | `client/handles.rs::Artifact` (no write API, `#[derive(Clone)]`) | `tests/conformance_artifacts.rs::extracted_artifact_outlives_the_message_it_came_from` (both) |
| `ArtifactWriter` is unique, not cloneable; sealing consumes its writable lifetime | conforms | `client/handles.rs::ArtifactWriter::seal_with_digest(mut self)` | `tests/artifacts.rs::seal_is_immune_to_live_writable_handles` (both) |
| Mapped slices cannot outlive their handle | conforms by construction: there is no mapping API; `ArtifactFile` owns its `Artifact` | `client/handles.rs::ArtifactFile` | `tests/bus_acceptance.rs::disconnect_releases_logical_ownership_without_mutating_open_bytes` (both) |
| Rust RAII automates releases | conforms | `client/handles.rs::OwnerGuard::drop` | `tests/artifacts.rs::fan_out_shares_one_object_and_the_last_consumer_collects` (both) |
| Other language bindings provide equivalent explicit close/context-manager behaviour | not-implemented (Rust only; section 1 says a binding "may" exist). Owner: a future binding | — | — |
| Garbage collection means reclaiming an unowned artifact, not inspecting game state | conforms | `router/state.rs::drop_roots` | `tests/conformance_artifacts.rs::collection_waits_for_every_retained_owner` (both) |

## 3. Addressing and identities

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| Every identity of the table exists: `routerId`, `clientId`/`clientIncarnation`, `connectionId`, `service`/`serviceIncarnation`, `callId`, `topic`/`topicIncarnation`/`topicSequence`, `deliveryId`, `artifactId`/`generation`, `ownerId` | conforms | `router/mod.rs::fresh_tag`, `router/state.rs` (`serial_id` for `conn`/`svc`/`top`/`sub`/`dlv`/`own`/`a`) | `tests/conformance_wire.rs::hello_reports_the_contract_digest_and_valid_limits` (both), `tests/rpc.rs::registration_is_exclusive_and_pinned` (both) |
| A fresh `routerId`/`storeId` per incarnation; old handles fail after restart | conforms | `router/mod.rs::Router::new` | `tests/artifacts.rs::router_restart_invalidates_old_handles` (both) |
| Reconnect creates a new `clientIncarnation`; v1 does not resume a connection's queues or delivery owners | conforms | `router/state.rs::hello` refuses a reused incarnation; `disconnect` releases everything | `tests/sol_review_races.rs::caller_disconnect_cleanup_works_before_and_after_consumption` |
| Identifiers are bounded ASCII; `Id` and `U64` match ipc-v1 | conforms: `^[a-z0-9][a-z0-9._-]{0,63}$`, `"0"\|[1-9][0-9]*` | `wire.rs::is_id`, `wire.rs::parse_u64` | `wire.rs::tests::scalars`, `tests/conformance_wire.rs::malformed_envelope_scalars_are_rejected` (both) |
| Service/topic names are 1..192 of `[a-z0-9._-]` with no empty dot-separated segment | conforms | `wire.rs::is_name` | `wire.rs::tests::scalars` |
| Exact names only; wildcard routing and queue groups deferred | conforms: routing is `HashMap` lookup by exact name. `Pattern::Prefix` is a launcher grant, never a route | `router/state.rs::services`/`topics`, `policy.rs::Pattern` | `tests/pubsub.rs::subscription_and_topic_validation` (both), `tests/rpc.rs::authority_is_enforced` (both) |
| One live registration owns a service name; duplicate registration fails; no implicit round-robin or replacement | conforms (`CONFLICT`) | `router/state.rs::op_register` | `tests/rpc.rs::registration_is_exclusive_and_pinned` (both), `tests/conformance_routing.rs::duplicate_registration_by_owner_itself_is_rejected` (both) |
| Registration returns the incarnation; callers pin it; a change fails with `TARGET_CHANGED` | conforms | `router/state.rs::op_call` | `tests/rpc.rs::registration_is_exclusive_and_pinned` (both), `tests/bus_acceptance.rs::no_automatic_retry_or_failover_onto_a_replacement_registration` (both) |
| An unpinned call reaches whoever holds the name now | conforms | `router/state.rs::op_call` (`expectedIncarnation: null`) | `tests/conformance_routing.rs::unpinned_call_after_incarnation_replacement_reaches_the_new_holder` (both) |
| `Worker.Hello` stays a domain RPC, distinct from transport negotiation | conforms by absence: `bus.hello` carries no role, capability or session field | `router/state.rs::hello` | `tests/wire.rs::hello_negotiation` (both) |
| Service/topic access is configured per participant by the launcher; naming a target is not authority | conforms | `policy.rs::{Policy, Grants}`, checked in every `op_*` | `tests/rpc.rs::authority_is_enforced` (both), `tests/pubsub.rs::subscription_and_topic_validation` (both) |
| Presentation subscribes without gaining authority to invoke Advance | conforms: `subscribe` and `call` are separate grants | `policy.rs::Grants` | `tests/rpc.rs::authority_is_enforced` (both) |
| One live connection per client id, and a client id's last incarnation may not be reused | deviates-allowed (narrowing): section 3 makes `callId` "unique ... for this client incarnation" and section 6 keeps serials "per connected client", which two live connections for one id would make ambiguous. Cost: one small record per client id ever seen | `router/state.rs::{ClientRecord, hello}` | `tests/sol_review_races.rs::pending_connections_are_bounded_and_hello_expires` |

## 4. Wire envelope and framing

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| Framing is `u32` little-endian JSON length, then UTF-8 JSON | conforms | `wire.rs::{read_frame, write_frame}` | `tests/conformance_wire.rs::length_prefix_is_little_endian` (both) |
| Envelope fields exactly `protocol`/`major`/`minor`/`id`/`replyTo`/`kind`/`op`/`body`/`attachments` | conforms | `wire.rs::Envelope::{decode, to_value}` | `tests/conformance_wire.rs::unknown_top_level_field_closes_the_connection` (both) |
| `ArtifactRef` fields `storeId`/`artifactId`/`generation`/`byteLength`/`contentType`/`digest` | conforms (`generation`/`byteLength` as U64 strings) | `wire.rs::ArtifactRef` | `tests/conformance_artifacts.rs::stale_store_incarnation_and_generation_are_rejected` (both) |
| `Attachment` is `{name, ref, ownerId}` | conforms | `wire.rs::Attachment` | `tests/conformance_artifacts.rs::allocate_write_seal_open_roundtrip_and_mismatches` (both) |
| Maximum total JSON envelope 65,536 bytes | conforms | `wire.rs::MAX_ENVELOPE_BYTES`, checked on decode and encode | `tests/conformance_wire.rs::frame_at_the_size_ceiling_is_accepted_one_byte_over_is_not` (both) |
| A delivery the router would build over the limit is refused at admission | conforms, and required by "Message length includes this wrapper" (section 5): admission sizes the delivery with the longest ids | `router/state.rs::frame_len` in `op_call`/`op_reply`/`op_publish` | `tests/sol_review_races.rs::unsent_oversized_call_rolls_back_its_local_slot`, `tests/wire.rs::envelope_size_limits` (both) |
| Up to 32 attachments, unique names; `contentType` nonempty ASCII <=127 | conforms | `wire.rs::{MAX_ATTACHMENTS, is_content_type}`, `Envelope::decode` | `tests/conformance_wire.rs::malformed_envelope_scalars_are_rejected` (both), `tests/artifacts.rs::quotas_are_enforced` (both) |
| No pixel/base64/checkpoint bytes in JSON; artifact sizes are independent of envelope size | conforms: bulk bytes only reach the store; `byteLength` is a string in the reference | `store.rs`, `wire.rs::ArtifactRef` | `tests/perf.rs` (1.2 MB frames, envelopes under 1 KiB) |
| Domain schemas enumerate every referenced artifact; generated bindings enforce it | not-implemented for domain schemas (the bus enforces its own attachment list). Owner: CONTRACT-01 / `fly-session-types` | `router/state.rs::check_attachments` enforces the bus half | `tests/conformance_artifacts.rs::forward_requires_the_source_owner_still_live` (both) |
| The router validates attachment declarations/ownership, not domain payload contents | conforms | `router/state.rs::{check_owned, check_attachments}`; `payload`/`outcome` stay opaque | `tests/conformance_artifacts.rs::owner_ids_are_scoped_to_their_connection` (both) |
| Commands have unique monotonically issued `msg-<U64>` ids per connection | conforms (strictly increasing) | `router/state.rs::handle` | `tests/conformance_wire.rs::{non_canonical_command_ids_are_rejected, non_increasing_command_ids_are_rejected}` (both) |
| Replies correlate with `replyTo`; notices and deliveries carry router-generated ids | conforms | `router/state.rs::{reply, outbound}` | `tests/conformance_wire.rs::non_null_reply_to_on_a_command_is_rejected` (both) |
| The router supplies authenticated sender/target metadata; senders cannot forge it in `body` | conforms on launcher-bound transports; the sender's `body` never supplies identity | `router/state.rs::{identity, Call::request_body}` | `tests/rpc.rs::raw_call_ids_and_forged_replies` (both), `tests/sol_review_races.rs::responder_survives_cancel_then_request_drop` |
| Identity is bound out of band before Hello; a mismatching Hello is refused before registration | conforms | `router/mod.rs::{serve_as, listen_unix_as}`, `state.rs::hello` | `tests/wire.rs::hello_refusals` (both) |
| Open/unbound transports are self-asserted, not authentication | deviates-allowed (explicit narrowing): section 1's "one trusted local deployment" and section 4's "The launcher provides expected client/registration privileges". `Policy::open()` is documented as test/trusted-only | `policy.rs::permits_unbound_transport`, `router/state.rs::add_conn` | `tests/sol_review_races.rs::pending_connections_are_bounded_and_hello_expires` |
| Reject duplicate JSON keys, invalid UTF-8, NaN/Infinity, unknown envelope fields, zero/oversize frames, invalid ranges | conforms | `wire.rs::{parse_json_strict, StrictVisitor, Fields::finish}`, `read_frame` | `tests/conformance_wire.rs::{duplicate_json_keys_are_rejected_at_every_depth, invalid_utf8_is_rejected, zero_length_frame_closes_the_connection, oversize_length_prefix_is_rejected_before_reading_body}` (both) |
| Read length before allocating | conforms | `wire.rs::read_frame` checks the prefix before `vec![0u8; len]` | `tests/conformance_wire.rs::oversize_length_prefix_is_rejected_before_reading_body` (both) |
| Handle partial reads/writes; serialise one writer per connection | conforms | `wire.rs::read_frame`, `router/mod.rs::{write_selected, write_loop}` (one writer task) | `tests/conformance_wire.rs::{a_frame_written_one_byte_at_a_time_still_decodes, a_reply_read_one_byte_at_a_time_still_decodes, truncated_frame_disconnects_cleanly}` (both) |
| No ancillary-FD tricks in the first file-backed implementation | conforms | `transport.rs` carries bytes only | — |
| A future memory backend keeps the same client/ownership API | not-implemented (future). Owner: a later storage backend | — | — |
| `bus.hello` body `{clientId, clientIncarnation, supportedMajors}`, no attachments; reply `{routerId, connectionId, selectedMajor, selectedMinor, contractDigest, limits}` | conforms | `router/state.rs::hello`, `limits.rs::Limits::to_json` | `tests/wire.rs::hello_negotiation` (both), `tests/conformance_wire.rs::{hello_refuses_attachments, hello_reports_the_contract_digest_and_valid_limits}` (both) |
| Refuse incompatible majors or identity mismatch before registration | conforms (`VERSION_MISMATCH`, `NOT_AUTHORIZED`) | `router/state.rs::hello` | `tests/conformance_wire.rs::hello_refuses_an_unsupported_major` (both), `tests/wire.rs::hello_refusals` (both) |
| Schema changes change `contractDigest` | conforms: the digest is the SHA-256 of `wire::CONTRACT`, which lists every operation, delivery and notice; the client refuses a router whose digest differs | `wire.rs::{CONTRACT, contract_digest}`, `client/mod.rs::connect` | `tests/conformance_wire.rs::memory_and_unix_negotiate_the_identical_contract` |
| Bodies reject unknown fields and `minor` must be 0 after hello | deviates-allowed (stricter than "unknown envelope fields", forbidden nowhere; section 4's envelope fixes `minor: 0`) | `wire.rs::Fields::finish`, `router/state.rs::handle` | `tests/wire.rs::body_errors_keep_the_connection` (both), `tests/sol_review_races.rs::client_rejects_unknown_reply_fields_and_invalid_direction_rules` |
| The connection reader dispatches replies and requests without blocking on user handlers | conforms: the reader hands `Request`/`Message` to unbounded per-handle channels and returns | `client/reactor.rs::{read_loop, on_delivery}` | `tests/pubsub.rs::saturated_subscriber_does_not_block_control` (both) |
| Artifact I/O and hashing run outside the routing critical section; no routing lock across slow I/O | conforms: `Outcome::Allocate`/`Seal` leave the lock, run on the blocking pool and re-enter; unlinks happen after the guard drops | `router/mod.rs::{read_loop, Inner::with_state}`, `store.rs::seal` | `tests/artifacts.rs::quotas_are_enforced` (both), `tests/perf.rs` (seal p50 1.3 ms, publish admission p50 0.4 ms) |

## 5. Operation registry

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| Replies are `{ok:true, value}` or `{ok:false, error:{code, message, dispatch}}` | conforms | `router/state.rs::reply_body`, `client/reactor.rs::parse_reply` | `tests/wire.rs::body_errors_keep_the_connection` (both) |
| All 19 commands of the table exist with the listed bodies and reply values | conforms | `router/state.rs::handle` dispatch table; `wire.rs::CONTRACT` | `tests/conformance_wire.rs` + `tests/{rpc,pubsub,artifacts}.rs` (both) |
| `rpc.responder.release {callId, requestDeliveryId} -> {released}` is added to the registry | deviates-allowed: section 4's "Changes to these draft schemas change contractDigest" (this is a draft), and it is what keeps section 6's "bounded call correlation metadata" bounded when a handler keeps a responder after dropping the request. Recorded as an amendment in bus-v1 section 12 | `router/state.rs::op_responder_release`, `client/handles.rs::ReplyGuard` | `tests/sol_rereview_regressions.rs::{dropping_last_attached_responder_settles_the_call, dropping_last_responder_clone_settles_the_call}` |
| Release batches carry 1..64 ids | conforms | `wire.rs::MAX_BATCH`, `Fields::array` | `tests/artifacts.rs::release_ids_are_watermarked` (both) |
| No attachments on management commands except `rpc.call`, `rpc.reply`, `publish` | conforms | `router/state.rs::handle` | `tests/wire.rs::body_errors_keep_the_connection` (both) |
| `accepted`/`routed`/`removed`/`declared`/`cleared`/`deleted`/`replayLatest` are booleans; `subscribers`/`replaced`/`released` are U64 counts | conforms | `router/state.rs` (`.into()` for bools, `to_string()` for counts) | `tests/pubsub.rs::bounded_fifo_and_atomic_backpressure` (both), `client/reactor.rs::validate_reply_value` |
| Released counts count newly released roots, so an idempotent repeat may report zero | conforms | `router/state.rs::{op_consumed, op_release}` | `tests/artifacts.rs::release_ids_are_watermarked` (both) |
| Queue/credit requests are integers 1..65535 and cannot exceed configured limits | conforms | `wire.rs::MAX_CREDIT`, `op_register`/`op_subscribe` quota checks | `tests/pubsub.rs::subscription_and_topic_validation` (both), `tests/rpc.rs::service_queue_backpressure` (both) |
| Method strings are 1..128 printable ASCII | conforms | `wire.rs::is_method` | `wire.rs::tests::scalars` |
| The call target is a service name; `expectedIncarnation` is the registration id | conforms | `router/state.rs::op_call` (`f.name`, `f.nullable_id`) | `tests/rpc.rs::registration_is_exclusive_and_pinned` (both) |
| Location grants are `{storeId, relativePath}` resolved under the configured root; absolute paths, parent traversal and symlink escapes are rejected | conforms | `store.rs::resolve`, opened `O_NOFOLLOW` | `store.rs::tests::resolve_refuses_escapes`, `tests/conformance_artifacts.rs::issued_locations_are_relative_and_contained` (both) |
| Locations are SDK-private and do not appear in an application's `ArtifactRef`; runtime paths are not committed into schemas | conforms: `Location` only ever appears in `artifact.allocate`/`artifact.open` replies | `wire.rs::Location`, `client/handles.rs::Artifact::open` | `tests/conformance_artifacts.rs::issued_locations_are_relative_and_contained` (both) |
| Deliveries carry exactly the listed bodies (`rpc.request`, `rpc.result`, `topic.message`) | conforms | `router/state.rs::{Call::request_body, Call::result_body, TopicMsg::body}` | `tests/conformance_wire.rs` + `client/reactor.rs::on_delivery` strict parse |
| `caller`/`responder` include clientId and clientIncarnation | conforms | `wire.rs::Identity` | `tests/rpc.rs::request_reply_roundtrip` (both) |
| Delivery attachment ownerIds are replaced by the recipient's deliveryId; source tokens are never delegated | conforms, and the client refuses a delivery whose attachment owner is not its delivery | `router/state.rs::attachments_with_owner`, `client/reactor.rs::attachments` | `tests/conformance_artifacts.rs::owner_ids_are_scoped_to_their_connection` (both) |
| `topicSequence` and counters are U64 strings; the router assigns the ids; the SDK exposes typed payloads plus handles | conforms | `router/state.rs::TopicMsg::body`, `client/handles.rs::Message` | `tests/pubsub.rs::retained_replay_clear_delete_and_incarnations` (both) |
| Required bounded notices: route removal, subscription closure, call failure | conforms | `router/state.rs::{remove_service, shutdown, op_responder_release}` | `tests/pubsub.rs::router_shutdown_closes_subscriptions_with_notices` (both), `tests/rpc.rs::service_disconnect_fails_calls` (both) |
| Who gets those notices, and when | deviates-allowed: the draft names the notices but not their audience. `route.removed` goes to callers with open calls on the removed registration, `subscription.closed` only at router shutdown (nothing else ends a subscription without the client's own act), and `connection.closing` is an added notice before every router-initiated close | `router/state.rs::{remove_service, shutdown, violation}` | `tests/wire.rs::malformed_frames_close_the_connection` (both) |
| If notice capacity is exhausted, close the connection rather than lose control-plane correctness | conforms | `router/state.rs::push_control` | `tests/wire.rs::control_lane_exhaustion_closes` (both) |

## 6. RPC behaviour

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| `call-<U64>` with increasing serials per connected client; reused or retired ids are rejected, never executed again | conforms: a syntactically valid id advances the watermark even when admission is refused | `router/state.rs::op_call` (`call_watermark`) | `tests/sol_review_races.rs::rejected_call_id_still_advances_monotonic_watermark`, `tests/rpc.rs::raw_call_ids_and_forged_replies` (both) |
| Reconnecting creates a new incarnation rather than reviving old calls | conforms | `router/state.rs::{hello, disconnect}` | `tests/sol_review_races.rs::caller_disconnect_cleanup_works_before_and_after_consumption` |
| An RPC targets one registered service, not a broadcast subject | conforms | `router/state.rs::op_call` | `tests/rpc.rs::request_reply_roundtrip` (both) |
| First-dispatch FIFO per caller and service; responses may complete out of order and correlate by callId | conforms | `router/state.rs::{Svc::queue, dispatch_rpc}` | `tests/rpc.rs::fifo_dispatch_and_out_of_order_completion` (both), `tests/conformance_routing.rs::out_of_order_replies_correlate_across_concurrent_callers` (both) |
| A service dispatcher can answer status concurrently with a long mutation | conforms | `router/state.rs::dispatch_rpc` (in-flight credits, not one-at-a-time) | `tests/bus_acceptance.rs::a_status_rpc_responds_while_another_handler_is_delayed` (both) |
| The router implements no frame barriers or numerical ordering | conforms by absence | `router/state.rs` (no domain fields) | `tests/integration.rs::session_over_one_router` (both) |
| Admission validates route, pinned incarnation, size, quotas and every source artifact owner, and establishes request-delivery roots atomically before accepting | conforms: every check precedes the first mutation, then roots, queue entry and reply | `router/state.rs::op_call` | `tests/conformance_artifacts.rs::rejected_call_creates_no_roots` (both), `tests/artifacts.rs::failed_admission_is_atomic` (both) |
| Rejection establishes no delivery and drops provisional roots | conforms | `router/state.rs::op_call` (validate-then-mutate) | `tests/conformance_artifacts.rs::rejected_call_creates_no_roots` (both) |
| An accepted call is not proof that its handler ran | conforms: `accepted` is admission only; the terminal outcome arrives as `rpc.result` or `call.failed` | `client/mod.rs::call`, `client/handles.rs::PendingCall` | `tests/rpc.rs::service_disconnect_fails_calls` (both) |
| Mark dispatched before any request bytes can reach the target; later transport loss is an unknown outcome | conforms: `Phase::Dispatched` and the delivery id are set when the frame is selected, before a byte is written | `router/state.rs::{next_frame, dispatch_rpc}` | `tests/sol_rereview_regressions.rs::shutdown_cancels_partial_rpc_request_before_reclaiming_attachment` |
| The service replies with its own owned handles; the router establishes caller-result ownership before accepting the reply | conforms | `router/state.rs::op_reply` (`check_attachments`, then `add_roots`, then the phase change) | `tests/rpc.rs::endpoint_cache_replays_artifact_results` (both) |
| Only bounded call correlation metadata is kept until the result is consumed or the caller detaches; not an indefinite result cache | conforms: the record dies with consumption, detachment, disconnect or the last responder release; reply capabilities share the per-client owner bound | `router/state.rs::{remove_call, dispatch_rpc, op_responder_release}` | `tests/sol_review_races.rs::{cancel_after_request_consumption_retires_correlation, dropping_service_with_buffered_requests_retires_every_call}` |
| A second `rpc.reply` for the same call is rejected, not routed twice | conforms (`CALL_GONE`) | `router/state.rs::op_reply` | `tests/rpc.rs::replies_are_single_and_independent_of_the_request_guard` (both) |
| Responding does not release the request's delivery guard | conforms | `client/handles.rs::{Request, Responder}` (separate guards) | `tests/rpc.rs::replies_are_single_and_independent_of_the_request_guard` (both) |
| No automatic retry or failover; never route a retry automatically to a restarted worker | conforms | `router/state.rs::{remove_service, disconnect}` fail calls instead of re-queueing | `tests/bus_acceptance.rs::no_automatic_retry_or_failover_onto_a_replacement_registration` (both) |
| A deadline belongs to the calling client; on timeout it may cancel | conforms: no budget on the wire; `result()` is cancel-safe under `tokio::time::timeout` | `client/handles.rs::PendingCall::{result, cancel}` | `tests/rpc.rs::cancellation_states` (both) |
| Domain retries use a fresh callId with the same domain requestId/body, pinned to the same incarnation; endpoint dedup supplies safe replay; the router does not infer it from method names | conforms | `client/mod.rs::call`; the router carries `payload` opaquely | `tests/bus_acceptance.rs::a_retransmission_repeats_the_domain_request_under_a_fresh_call_id` (both), `tests/rpc.rs::endpoint_cache_replays_artifact_results` (both) |
| Queued cancellation releases its queued artifact roots and returns `cancelled-before-dispatch` | conforms | `router/state.rs::op_cancel` -> `remove_call` -> `drop_roots` | `tests/conformance_routing.rs::cancel_before_dispatch_releases_queued_artifact_roots` (both) |
| After dispatch, return `execution-unknown` and keep the recipient's delivery alive until consumed or disconnected | conforms | `router/state.rs::op_cancel` (`detached = true`, delivery untouched) | `tests/rpc.rs::cancellation_states` (both), `tests/conformance_routing.rs::caller_disconnect_detaches_dispatched_call_but_service_keeps_serving` (both) |
| A later reply to a detached call returns `routed:false` with no caller-result roots; the service still owns any retained result | conforms | `router/state.rs::op_reply` (`call.detached` branch, before `add_roots`) | `tests/conformance_routing.rs::cancel_after_dispatch_then_late_reply_with_artifact_is_not_routed` (both) |
| A terminal result already admitted makes cancellation report `completed`; the client drains and consumes it | conforms | `router/state.rs::op_cancel` (`Phase::Replied`), `client/reactor.rs::on_delivery` consumes an abandoned result | `tests/rpc.rs::{cancellation_states, dropped_call_is_cancelled_and_late_result_consumed}` (both) |
| A retired or unknown correlation reports `call-gone`; those four strings are the complete enum | conforms | `router/state.rs::op_cancel`, `client/handles.rs::CancelState` | `tests/rpc.rs::cancellation_states` (both), `client/reactor.rs::validate_reply_value` |
| No cancel state authorises re-execution; cancelling a future does not abandon incoming delivery ownership | conforms | `client/handles.rs::CallGuard::drop` (best-effort cancel, result still consumed) | `tests/rpc.rs::dropped_call_is_cancelled_and_late_result_consumed` (both), `tests/sol_review_races.rs::reply_racing_cancel_has_only_the_two_contract_outcomes` |
| Endpoint replay caches Artifact handles plus payload, not bare references, and owns holds until eviction | conforms (SDK support; the discipline is the endpoint's) | `client/handles.rs::Artifact::retain`, `ArtifactWriter::seal` returns a hold | `tests/rpc.rs::endpoint_cache_replays_artifact_results` (both), `tests/bus_acceptance.rs::a_lost_result_leaks_no_roots_and_the_endpoint_cache_still_replays` (both) |
| Re-delivery gets new delivery ids pointing to the same immutable bytes | conforms | `router/state.rs::dispatch_rpc` (fresh `dlv-<n>` per delivery) | `tests/rpc.rs::endpoint_cache_replays_artifact_results` (both) |
| An expired domain cache returns `RESULT_EXPIRED` | not-implemented: a domain code, not a transport code. Owner: `fly-session-rpc` (ipc-v1) | — | — |

## 7. Pub/sub semantics

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| Topic declaration teaches the router nothing about meaning; no hardcoded frame/brain topics | conforms | `router/state.rs::{Topic, op_declare}` | `tests/pubsub.rs::retained_replay_clear_delete_and_incarnations` (both) |
| `latest`: one queued value, replacing only an undelivered one; replacement releases that entry's roots; delivered or in-use messages are never reclaimed early; maxQueued is exactly 1 | conforms | `router/state.rs::{op_publish (latest branch), op_subscribe}` | `tests/pubsub.rs::latest_coalesces_only_undelivered_values` (both), `tests/conformance_artifacts.rs::latest_mode_holds_at_most_two_roots_delivered_plus_queued` (both) |
| `bounded`: FIFO, no coalescing or silent loss; when capacity is unavailable, reject with `BACKPRESSURE` before admitting any delivery | conforms | `router/state.rs::op_publish` (pre-checks every subscriber) | `tests/pubsub.rs::bounded_fifo_and_atomic_backpressure` (both), `tests/conformance_routing.rs::bounded_overflow_rolls_back_all_artifact_roots` (both) |
| maxInFlight credits return only on `delivery.consumed`, not on socket write completion | conforms | `router/state.rs::release_owner` (credit returned when the owner is released) | `tests/pubsub.rs::credits_return_only_on_consume` (both), `tests/conformance_routing.rs::bounded_credit_waits_for_every_extracted_artifact` (both) |
| A latest subscriber with all credits in use still has one replaceable queued value | conforms | `router/state.rs::{Sub::queue, dispatch_topic}` (queue and credits are separate) | `tests/bus_acceptance.rs::both_transports_produce_equivalent_behaviour_traces` (events 21 and 24: `publish replaced=1`, then `latest seq=3 replaced=1`) |
| Atomic subscriber/retention snapshot at admission; validate and reserve every queue entry and owner budget before accepting | conforms: one mutex, validate-then-mutate | `router/state.rs::op_publish` | `tests/artifacts.rs::failed_admission_is_atomic` (both) |
| A bounded overflow rejects the whole publish: no partial fan-out, no retained-latest update | conforms | `router/state.rs::op_publish` | `tests/conformance_routing.rs::bounded_overflow_rolls_back_all_artifact_roots` (both) |
| On acceptance, one `topicSequence` and roots for every delivery and the optional retained value | conforms; a refused publication spends no sequence number | `router/state.rs::op_publish` (`t.sequence += 1` after the checks) | `tests/pubsub.rs::bounded_fifo_and_atomic_backpressure` (both) |
| Different topics have no total ordering; multiple publishers follow router acceptance order | conforms: per-topic sequence only | `router/state.rs::Topic::sequence` | `tests/pubsub.rs::retained_replay_clear_delete_and_incarnations` (both) |
| The publication reply counts accepted subscriptions and replaced queue entries, not consumers that processed data | conforms | `router/state.rs::op_publish` reply | `tests/pubsub.rs::latest_coalesces_only_undelivered_values` (both) |
| `replaced` on a delivery reports how many undelivered messages were coalesced since that subscription's preceding delivery | conforms | `router/state.rs::{Sub::replaced, dispatch_topic}` (taken at dispatch) | `tests/pubsub.rs::latest_coalesces_only_undelivered_values` (both) |
| Optional `retained:latest` holds one last message and its artifacts independent of subscribers | conforms | `router/state.rs::op_publish` (retain branch) | `tests/conformance_artifacts.rs::retained_topic_value_holds_a_root_independent_of_subscribers` (both) |
| `replayLatest` enqueues the retained value before subsequent accepted publications; bounded preserves the order, latest may coalesce it | conforms | `router/state.rs::op_subscribe` (replay is enqueued under the subscribe lock) | `tests/conformance_routing.rs::latest_replay_is_ordered_ahead_of_a_racing_publish` (both), `tests/pubsub.rs::retained_replay_clear_delete_and_incarnations` (both) |
| Replay uses the original topicSequence, a fresh deliveryId and explicit roots | conforms | `router/state.rs::op_subscribe` (`add_roots`, the same `Arc<TopicMsg>`) | `tests/pubsub.rs::retained_replay_clear_delete_and_incarnations` (both) |
| Without retention, a zero-subscriber publication retains no ownership after admission | conforms | `router/state.rs::op_publish` | `tests/pubsub.rs::zero_subscriber_publish_retains_nothing` (both) |
| Clearing a topic releases only its retained root, not active consumers | conforms | `router/state.rs::op_clear` | `tests/conformance_routing.rs::cleared_topic_gives_no_replay_until_a_fresh_publish` (both) |
| Topic count and retained bytes are capped | conforms; `max_retained_bytes` is added to the draft's table because this sentence requires it | `limits.rs::{max_topics, max_retained_bytes}`, `router/state.rs::{op_declare, op_publish}` | `tests/pubsub.rs::topic_and_retention_quotas` (both) |
| No durable replay, automatic redelivery or exactly-once claim | conforms by absence | `router/state.rs` (queues are in memory and die with the connection) | `tests/artifacts.rs::router_restart_invalidates_old_handles` (both) |
| Deleting or redeclaring a topic creates a fresh topicIncarnation; a reset sequence cannot be read as continuation | conforms | `router/state.rs::{op_delete, op_declare}` | `tests/pubsub.rs::retained_replay_clear_delete_and_incarnations` (both) |
| Old subscription deliveries keep their original incarnation and ownership until consumed | conforms | `router/state.rs::drop_subscription` (matches on the incarnation), delivery bodies carry it | `tests/pubsub.rs::unsubscribe_discards_queue_but_not_deliveries` (both) |
| `topic.delete` only with no subscribers | conforms (`CONFLICT`) | `router/state.rs::op_delete` | `tests/pubsub.rs::subscription_and_topic_validation` (both) |
| Bus admission, message consumption and durable storage acknowledgment are three different events | conforms: `publish` returns admission counts, `delivery.consumed` is separate, and there is no storage ack in the bus | `router/state.rs::{op_publish, op_consumed}` | `tests/pubsub.rs::credits_return_only_on_consume` (both) |
| A topic must be declared before publish or subscribe | deviates-allowed: the draft is silent on undeclared topics, while "Topic count and retained bytes are capped" and `topic.declare`'s "conflicting settings fail" both imply a registry a publication cannot create by accident. `NO_TOPIC` names the refusal (amendment, section 12); `topic.delete` of an unknown topic still answers `deleted:false` | `router/state.rs::{op_publish, op_subscribe, op_clear}` | `tests/pubsub.rs::subscription_and_topic_validation` (both) |

## 8. Artifact lifecycle and garbage collection

### 8.1 Immutable object lifecycle

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| `ALLOCATED/WRITING -> SEALED -> owned -> COLLECTED`, and an abandoned or disconnected writer goes straight to COLLECTED | conforms | `router/state.rs::{ArtState, abandon_writer, finish_seal}` | `tests/artifacts.rs::{writer_drop_releases_staging, disconnect_releases_all_but_retained}` (both), `tests/conformance_artifacts.rs::disconnect_abandons_an_unsealed_writer` (both) |
| `ArtifactRef` is an identity, not an address or authority; opening needs a current root on that connection | conforms | `router/state.rs::{check_owned, op_open}` | `tests/conformance_artifacts.rs::owner_ids_are_scoped_to_their_connection` (both) |
| storeId is the store incarnation; old handles fail after restart | conforms (`ARTIFACT_GONE`) | `router/state.rs::check_owned` | `tests/artifacts.rs::router_restart_invalidates_old_handles` (both) |
| No artifact id or inode reuse; `generation` is 1 | conforms | `wire.rs::GENERATION`, `router/state.rs::next_artifact` (monotonic) | `tests/conformance_artifacts.rs::stale_store_incarnation_and_generation_are_rejected` (both) |
| Content hashes optional for live frames, mandatory where a domain contract says so | conforms: both paths exist and the router verifies what it is given | `client/handles.rs::ArtifactWriter::seal_with_digest`, `store.rs::copy_exact` | `tests/artifacts.rs::seal_checks_length_and_digest` (both) |
| The first backend is runtime-configured local files, optionally on tmpfs | conforms | `store.rs::Store::create` under `RouterConfig::store_root` | `store.rs::tests::orphans_are_removed_and_live_stores_kept` |
| The producer writes staging storage outside the message stream | conforms | `store.rs::create_staging`, `client/mod.rs::Artifacts::allocate` | `tests/artifacts.rs::allocate_write_seal_read` (both) |
| Seal closes writable handles in the SDK, checks length and digest, then finishes an immutable store-owned object before acknowledging | conforms: a writable descriptor kept after sealing reaches only the unlinked staging inode | `client/handles.rs::seal_with_digest` (drops the file first), `store.rs::seal` (fresh 0444 inode) | `tests/artifacts.rs::seal_is_immune_to_live_writable_handles` (both), `tests/conformance_artifacts.rs::seal_is_immutable_despite_a_stale_writable_handle` (both) |
| A copy into a fresh sealed inode is allowed; account for both allocations during sealing | conforms | `router/state.rs::op_seal` (`store_bytes += len` for the copy, released in `finish_seal`) | `tests/artifacts.rs::quotas_are_enforced` (both) |
| No per-frame fsync for transient media | conforms by absence | `store.rs` | `tests/perf.rs` (seal p50 1.3 ms for 1.2 MB) |
| Consumers resolve a readLocation through `artifact.open` and read it read-only | conforms | `router/state.rs::op_open`, `store.rs::open_read` | `tests/conformance_artifacts.rs::allocate_write_seal_open_roundtrip_and_mismatches` (both) |
| Locations are private grants, not placed in application bodies or public feeds | conforms | `router/state.rs::op_open` reply only | `tests/conformance_artifacts.rs::issued_locations_are_relative_and_contained` (both) |
| All filesystem access stays behind the client Artifact API; no second bulk-transfer server | conforms in the API. The store is only as private as the OS user, which the crate's Limitations section states | `client/handles.rs`, `store.rs` | `store.rs::tests::resolve_refuses_escapes` |

### 8.2 What owns an artifact

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| Roots: producer hold/active writer, accepted queued delivery, in-flight delivery, retained latest, explicit hold | conforms, all five | `router/state.rs::{Owner, add_roots, drop_roots}` | `tests/conformance_artifacts.rs::{queued_deliveries_hold_roots_before_dispatch, collection_waits_for_every_retained_owner, retained_topic_value_holds_a_root_independent_of_subscribers}` (both) |
| Seal transfers the unique writer into a hold | conforms; the seal reply reuses the writer's own ownerId to express exactly that transfer | `router/state.rs::finish_seal` | `tests/artifacts.rs::allocate_write_seal_read` (both) |
| Admission creates destination roots before the sender may relinquish source roots | conforms: the SDK holds every source `OwnerGuard` until the router has answered | `client/reactor.rs::OutCommand::keep`, `router/state.rs::{op_call, op_reply, op_publish}` | `tests/conformance_artifacts.rs::forward_requires_the_source_owner_still_live` (both) |
| A timeout must not drop a source guard while an unsent operation might still be admitted; the client keeps the guard until the transport outcome is known | conforms: an unsent command's guards travel with it and are released only when it fails or is answered | `client/reactor.rs::{next_outgoing, fail_all}` | `tests/sol_review_races.rs::unsent_oversized_call_rolls_back_its_local_slot`, `tests/artifacts.rs::abandoned_futures_do_not_leak_owners` (both) |
| Every envelope lists its complete artifact set; duplicates in one delivery count once | conforms | `router/state.rs::{check_attachments, dedup}` | `tests/conformance_artifacts.rs::allocate_write_seal_open_roundtrip_and_mismatches` (both) |
| A retained topic and several consumers can reference the same bytes; the router updates metadata only and never copies bytes for fan-out | conforms | `router/state.rs::op_publish` (`Arc<TopicMsg>` plus root counts) | `tests/artifacts.rs::fan_out_shares_one_object_and_the_last_consumer_collects` (both), `tests/perf.rs` (store peak 3.7 MB for three consumers) |

### 8.3 Consumed means no remaining use

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| The incoming message owns a shared DeliveryGuard; extracting an Artifact clones it; dropping the message alone does not consume the delivery | conforms | `client/handles.rs::{OwnerGuard, Message::artifact}` | `tests/artifacts.rs::extracted_artifacts_outlive_their_message` (both), `tests/conformance_artifacts.rs::extracted_artifact_outlives_the_message_it_came_from` (both) |
| Local handle clones need no bus round trip; dropping the last guard queues `delivery.consumed` on a bounded control lane | conforms | `client/handles.rs::OwnerGuard::drop`, `client/reactor.rs::push_control` | `tests/pubsub.rs::credits_return_only_on_consume` (both) |
| Ownership is at delivery granularity; independent retention needs `artifact.retain` before the guard is dropped | conforms | `client/handles.rs::Artifact::retain` | `tests/conformance_artifacts.rs::explicit_retain_outlives_the_original_hold` (both) |
| A domain acknowledgment implicitly drops nothing | conforms by absence: only a guard drop or an explicit release ends ownership | `client/handles.rs::OwnerGuard` | `tests/rpc.rs::endpoint_cache_replays_artifact_results` (both) |
| An allocation or seal grant that arrives after its caller abandoned the future is still processed and released; no owner the application never saw is leaked | conforms: the reactor builds the handle, so an undelivered reply drops it | `client/reactor.rs::{complete, Hook::Owner}` | `tests/artifacts.rs::abandoned_futures_do_not_leak_owners` (both) |
| An in-progress seal has a bounded I/O hold; on producer disconnect it cleans up and never publishes an ownerless object | conforms | `router/state.rs::finish_seal` (`owner_live` check), `router/mod.rs::read_loop` seal task | `tests/conformance_artifacts.rs::disconnect_abandons_an_unsealed_writer` (both) |
| Dropping a response future is not consumption: the client owns queued results until surfaced, discarded or disconnected | conforms | `client/reactor.rs::{CallSlot, on_delivery}` | `tests/rpc.rs::dropped_call_is_cancelled_and_late_result_consumed` (both) |
| Receivers await asynchronous CPU/GPU use before releasing the guard | conforms as far as the API can enforce: the guard lives as long as any `Artifact`/`ArtifactFile` clone | `client/handles.rs::{Artifact, ArtifactFile}` | `tests/conformance_routing.rs::bounded_credit_waits_for_every_extracted_artifact` (both) |
| A pointer from a mapping cannot outlive its Artifact; FFI wrappers enforce it | not-implemented: no mapping and no FFI surface exists. Owner: a future mmap or binding | — | — |
| Release commands are batched, idempotent and scoped to the owning connection | conforms | `router/state.rs::{op_release, op_consumed}`, `client/reactor.rs::take_batch` | `tests/artifacts.rs::owners_are_scoped_to_their_connection` (both) |
| Delivery and hold ids use monotonic per-connection serials with separate watermarks; a retired id is a no-op, a never-issued one an error; no tombstone per frame | conforms | `router/state.rs::{Conn::delivery_issued, Conn::hold_issued, op_consumed, op_release}` | `tests/artifacts.rs::release_ids_are_watermarked` (both) |
| Control-lane exhaustion closes the connection instead of losing releases | conforms on both sides | `router/state.rs::push_control`, `client/reactor.rs::push_control` | `tests/wire.rs::control_lane_exhaustion_closes` (both) |

### 8.4 Crash, disconnect and safe physical reclamation

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| On disconnect: unregister services and subscriptions, cancel queued deliveries, release that connection's writers, holds and delivery roots | conforms | `router/state.rs::disconnect` | `tests/artifacts.rs::disconnect_releases_all_but_retained` (both), `tests/conformance_artifacts.rs::{disconnect_releases_an_explicit_hold, disconnect_releases_queued_and_dispatched_deliveries}` (both) |
| Retained topic roots stay router-owned | conforms | `router/state.rs::disconnect` (topics untouched) | `tests/conformance_routing.rs::subscriber_disconnect_releases_queued_and_delivered_artifacts_but_not_retention` (both) |
| Late replies and releases cannot attach to a new connection or service incarnation | conforms: owners are per connection and calls remember their service incarnation | `router/state.rs::{check_owned, op_reply, remove_service}` | `tests/artifacts.rs::owners_are_scoped_to_their_connection` (both), `tests/rpc.rs::raw_call_ids_and_forged_replies` (both) |
| Teardown reclaims a connection's roots without racing the frame it is writing: either a complete frame precedes the reclamation or a partial one is cut and nothing more is appended | conforms, **fixed on this branch** (see the section 9 row on starvation). Teardown now marks the stream closing once, before waiting for the poll already in progress, so only that one poll can still write and every later one is refused | `router/state.rs::WriteGate`, `router/mod.rs::write_selected`, `router/state.rs::close_conn` | `tests/sol_rereview_regressions.rs::{teardown_waits_for_an_active_transport_poll_before_reclaiming, shutdown_does_not_deliver_a_frame_after_releasing_its_owner, protocol_close_cancels_partial_topic_frame_before_reclaiming_attachment, shutdown_cancels_partial_rpc_request_before_reclaiming_attachment, shutdown_cancels_partial_rpc_result_before_reclaiming_attachment}` |
| GC removes the registry entry and unlinks the sealed object after its final root is gone | conforms; the unlink happens after the routing lock is released | `router/state.rs::drop_roots`, `router/mod.rs::Inner::with_state` | `tests/artifacts.rs::fan_out_shares_one_object_and_the_last_consumer_collects` (both) |
| Existing mappings stay valid until the OS closes them; never overwrite the inode or reuse its bytes | conforms: sealed files are 0444, written once and only unlinked | `store.rs::{seal, remove}` | `tests/bus_acceptance.rs::disconnect_releases_logical_ownership_without_mutating_open_bytes` (both) |
| Logical reclamation is not proof of physical release; measurements include OS mappings and client memory | conforms: `RouterStats` is documented as logical, and the measurement reports process RSS | `router/state.rs::RouterStats`, `tests/perf.rs` | `tests/perf.rs` (RSS 11 to 16 MB) |
| No TTL may reclaim a live owned artifact | conforms by absence: nothing in the router expires an owned root | `router/state.rs` | `tests/conformance_artifacts.rs::collection_waits_for_every_retained_owner` (both) |
| Limits may disconnect a consumer but cannot overwrite memory under a renderer | conforms: exhaustion closes the connection, which releases roots; bytes are never rewritten | `router/state.rs::push_control`, `store.rs` | `tests/wire.rs::control_lane_exhaustion_closes` (both) |
| Future pooled shared memory must prove equivalent lifetime/generation safety | not-implemented (deferred by the draft). Owner: a later storage backend | — | — |
| Router restart makes a new routerId/storeId, loses routes, queues and retention, and invalidates old handles | conforms | `router/mod.rs::Router::new`, `store.rs::Store::create` | `tests/artifacts.rs::router_restart_invalidates_old_handles` (both) |
| Orphan files from a stopped router are cleaned without being treated as durable checkpoints | conforms, at the next router start on the same root (a `flock`-free marked directory) | `store.rs::clean_orphans` | `store.rs::tests::orphans_are_removed_and_live_stores_kept` |
| Live sessions fail their epoch and use coherent recovery | not-implemented: a session responsibility. Owner: STATE-01 / step-v1 | — | — |

## 9. Bounds, scheduling and failure reporting

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| Every default of the table, exactly: clients/services/topics 64/256/512; subscriptions 128 per client and 1024 total; control envelope 64 KiB; active calls 64; service queued/in-flight 16/16; latest 1/2; bounded 64/16; active owners 256; store 512 MiB and 128 MiB per object; per-client queued envelope bytes 1 MiB; reserved lane 128 frames and 1 MiB | conforms | `limits.rs::Limits::default`, `wire.rs::MAX_ENVELOPE_BYTES` | `tests/conformance_wire.rs::hello_reports_the_contract_digest_and_valid_limits` (both), `tests/pubsub.rs::topic_and_retention_quotas` (both) |
| Limits are configured explicitly and a configuration the router could not honour is refused | conforms | `limits.rs::Limits::validate`, `router/mod.rs::Router::new` | `tests/rpc.rs::active_call_limit` (both), `tests/artifacts.rs::quotas_are_enforced` (both) |
| The per-client queued envelope byte budget counts `bounded` subscriptions only, and latest slots are bounded at subscriptions x 64 KiB instead | conforms to the amended table. The draft's single row was the audit's contradiction 1: this section also forbids a latest spectator from being the reason a publication is refused, so its slot cannot sit in a budget whose overflow rejects one. The coordinator amended the row and gave latest slots their own (bus-v1 section 12, 2026-09-22) | `limits.rs::max_queued_bytes_per_client`, `router/state.rs::op_publish` | `tests/bus_acceptance.rs::a_latest_subscriber_never_refuses_a_publication` (both), `tests/sol_review_races.rs::{retained_replay_obeys_bounded_queue_byte_quota, latest_replay_remains_bounded_outside_the_bounded_byte_pool}` |
| Reserve an owner allowance for lifecycle and results separately from ordinary telemetry | conforms | `limits.rs::reserved_owners_per_client`, `router/state.rs::{ordinary_budget_left, dispatch_rpc}` | `tests/conformance_artifacts.rs::artifact_bounds_and_owner_budget_are_enforced` (both) |
| Memory quotas account for staging, seal copies, queued deliveries and caches | conforms | `router/state.rs::{op_allocate, op_seal, Conn::queued_bytes}` | `tests/artifacts.rs::quotas_are_enforced` (both) |
| Ownership metadata is bounded even when many roots share one artifact | conforms: a root is a counter, and owners are bounded per client | `router/state.rs::{Art::roots, ordinary_budget_left}` | `tests/conformance_artifacts.rs::artifact_bounds_and_owner_budget_are_enforced` (both) |
| Disk-full, allocation failure or hash mismatch returns a typed artifact error and cleans provisional storage and roots | conforms (`QUOTA_EXCEEDED`, `STORE_FAILURE`, `ARTIFACT_MISMATCH`) | `router/state.rs::{op_allocate, finish_allocate, op_seal, finish_seal}`, `store.rs::seal` | `tests/artifacts.rs::seal_checks_length_and_digest` (both), `tests/conformance_artifacts.rs::allocate_write_seal_open_roundtrip_and_mismatches` (both) |
| The router fairly services clients | deviates-allowed: fairness is Tokio's scheduling plus a yield every 32 commands from one connection, and round robin between a connection's services and subscriptions. The draft sets no fairness metric; the crate's Limitations section calls this modest | `router/mod.rs::read_loop`, `router/state.rs::{dispatch_rpc, dispatch_topic}` | `tests/pubsub.rs::saturated_subscriber_does_not_block_control` (both) |
| Replies, release, cancellation and route-health control cannot be starved by telemetry | conforms: control first, then RPC, then topic data, with topic data given a turn after 16 higher-priority frames | `router/state.rs::{next_frame, TOPIC_STARVATION_LIMIT}` | `tests/sol_rereview_regressions.rs::{fair_topic_insertion_preserves_router_envelope_order, sdk_accepts_fair_topic_insertion_through_saturated_control_backlog}`, `tests/pubsub.rs::saturated_subscriber_does_not_block_control` (both) |
| ... and neither can connection teardown be starved by the frame it is waiting for | **was deviates-must-fix, fixed on this branch**. The write gate was a plain mutex held across each synchronous transport poll, so a writer sending a frame one byte per poll re-acquired it hundreds of times while teardown waited for it, and could finish a whole delivery before teardown got in: `teardown_waits_for_an_active_transport_poll_before_reclaiming` failed 6 runs out of 6 in release and about 1 in 5 in debug. The gate now separates "teardown has begun" (a flag set once, without waiting) from "a poll is in progress" (a condvar teardown waits on), so teardown's window is one poll instead of a whole frame, and no byte can follow it. No public signature changed | `router/state.rs::WriteGate`, `router/mod.rs::write_selected` | `tests/sol_rereview_regressions.rs::teardown_waits_for_an_active_transport_poll_before_reclaiming` (rewritten: a 50 KB delivery the resumed writer cannot finish, and a channel instead of a sleep) |
| Preserve FIFO for calls to a target despite lane scheduling | conforms: the service queue is FIFO and lane choice never reorders it | `router/state.rs::dispatch_rpc` | `tests/rpc.rs::fifo_dispatch_and_out_of_order_completion` (both) |
| Classification is an explicit generic envelope operation or policy, not a topic-name heuristic | conforms by construction: `next_frame` chooses a lane from the queue an item sits in (control, then RPC, then topic), and neither it nor `pop_control`/`dispatch_rpc`/`dispatch_topic` reads a service or topic name. A name reaches the scheduler only as opaque bytes inside an already-classified frame | `router/state.rs::{next_frame, pop_control, dispatch_rpc, dispatch_topic}` | `tests/bus_acceptance.rs::a_topic_named_like_a_notice_is_still_classified_as_topic_data` (both), and `tests/sol_rereview_regressions.rs::fair_topic_insertion_preserves_router_envelope_order` for the lane order itself |
| No indefinite wait inside the router on subscriber readiness or artifact I/O | conforms: a slow reader stalls only its own writer task; I/O leaves the lock | `router/mod.rs::{write_loop, read_loop}` | `tests/pubsub.rs::saturated_subscriber_does_not_block_control` (both) |
| Admission is bounded; rejected callers choose their own policy | conforms | `router/state.rs::{op_call, op_publish}` | `tests/rpc.rs::service_queue_backpressure` (both) |
| The thirteen transport error codes exist with those names | conforms | `error.rs::ErrorCode` | `tests/wire.rs::body_errors_keep_the_connection` (both) |
| Three more codes: `CONFLICT`, `NO_TOPIC`, `ARTIFACT_MISMATCH` | deviates-allowed: "Transport errors **include** ..." is not an exhaustive list, and each names a refusal the draft requires but leaves unnamed. Recorded as an amendment in bus-v1 section 12 | `error.rs::ErrorCode` | `tests/pubsub.rs::subscription_and_topic_validation` (both), `tests/artifacts.rs::seal_checks_length_and_digest` (both) |
| Before admission report `not-dispatched`; once dispatch might have occurred report `dispatched` or `unknown` conservatively | conforms | `error.rs::BusError::new` (not-dispatched by default), `router/state.rs` dispatched notices, `client/reactor.rs::fail_all` (unknown) | `tests/rpc.rs::{cancellation_states, service_disconnect_fails_calls}` (both), `tests/sol_review_races.rs::writer_failure_terminates_reader_and_pending_work` |
| Bounded subscriptions can reject a publication; latest spectators cannot hold a session transaction indefinitely | conforms: a latest subscriber never causes `BACKPRESSURE` | `router/state.rs::op_publish` (the latest branch skips every capacity check) | `tests/bus_acceptance.rs::a_latest_subscriber_never_refuses_a_publication` (both: 100 publications of 60 KB into one unconsumed slot, six times the bounded pool, none refused, 98 coalesced), with `tests/pubsub.rs::{bounded_fifo_and_atomic_backpressure, saturated_subscriber_does_not_block_control}` (both) for the bounded half |
| Sustained pinned-artifact quota exhaustion is surfaced as pressure, not solved by freeing live data | conforms: `QUOTA_EXCEEDED`, never eviction | `router/state.rs::{op_allocate, op_seal}` | `tests/artifacts.rs::quotas_are_enforced` (both) |
| Session and application policies choose disconnect, pause or fail; the router does not know which | conforms by absence | `router/state.rs` | `tests/pubsub.rs::saturated_subscriber_does_not_block_control` (both) |

## 10. Native-frame bandwidth check

| Requirement | Status | Code | Test |
| --- | --- | --- | --- |
| 640x480 RGBA is 1,228,800 bytes; 73.728 MB/s at 60 fps is the planning dimension | conforms as measured, not as a claim | `tests/perf.rs::{W, H, HZ}` | `tests/perf.rs` (120 frames in 2.00 s, 0 late) |
| Two agent workers plus one presentation consumer read the same immutable frame object | conforms | `router/state.rs::op_publish` (roots, not copies) | `tests/perf.rs` (three consumers, store peak 3.7 MB = three 1.2 MB objects in flight, not nine) |
| Bus messages contain only references; no 3x byte fan-out through the router | conforms | `wire.rs::Attachment` | `tests/artifacts.rs::fan_out_shares_one_object_and_the_last_consumer_collects` (both), `tests/perf.rs` |
| Readers still incur memory traffic; renderer readback and seal copying remain real costs | conforms, and both are now measured separately | `tests/perf.rs` (`producer copy into staging`, `seal`, `consumer readback`) | `tests/perf.rs` |
| No claim of zero-copy capture or measured host capacity | conforms: the measurement section below says so in those words | `tests/perf.rs` header | — |
| Nothing game-specific, no rendering, publishing or sampling logic in Flybus | conforms: no crate in the workspace depends on flybus yet, and the crate names no game, brain or stream concept | `crates/flybus/**` | `tests/integration.rs::session_over_one_router` (both; the domain lives in the test) |

## 11. Acceptance tests and implementation sequence

| bus-v1 item | Status | Test |
| --- | --- | --- |
| 1. Wire/router: schema, framing, Hello, exclusive routes, pinned incarnations, request/reply, disconnect, bounds; in-memory passes the same tests as Unix sockets | conforms | `tests/conformance_wire.rs` (45), `tests/wire.rs` (12), `tests/rpc.rs` (26), all `both_transports!`; `tests/bus_acceptance.rs::both_transports_produce_equivalent_behaviour_traces` |
| 2. Pub/sub: exact topics, FIFO/bounded rejection, latest coalescing, retained replay and clear, atomic fan-out, fair control/reply delivery under a saturated subscriber | conforms | `tests/pubsub.rs` (20), `tests/conformance_routing.rs` (24) |
| 3. Artifacts: allocate/seal/read; publication before seal fails; fan-out owns one object; the last consumer releases; a retained extracted frame survives a message drop | conforms | `tests/artifacts.rs` (28), `tests/conformance_artifacts.rs` (36) |
| 4. Faults: sender drops after admission, consumer dies mid-read, reply lost, queued frame replaced, subscription closes with in-use deliveries, router restarts, old release arrives; no double-free, use-after-reuse, unbounded tombstones or hidden replay | conforms | `tests/conformance_routing.rs::{caller_disconnect_detaches_dispatched_call_but_service_keeps_serving, subscriber_disconnect_releases_queued_and_delivered_artifacts_but_not_retention}`, `tests/artifacts.rs::{release_ids_are_watermarked, router_restart_invalidates_old_handles}`, `tests/bus_acceptance.rs::{a_lost_result_leaks_no_roots_and_the_endpoint_cache_still_replays, disconnect_releases_logical_ownership_without_mutating_open_bytes}`, `tests/sol_rereview_regressions.rs` (11) |
| 5. RPC cache: an endpoint retains an artifact-bearing result, the original caller consumes it, a domain retry still returns valid bytes, eviction drops the last hold | conforms | `tests/rpc.rs::endpoint_cache_replays_artifact_results` (both), `tests/bus_acceptance.rs::a_retransmission_repeats_the_domain_request_under_a_fresh_call_id` (both) |
| 6. Integration: two parallel fake agents, a complete-batch environment RPC, committed snapshot publication and a deliberately slow presentation consumer on one router | conforms | `tests/integration.rs::session_over_one_router` (both) |
| 7. Performance: 640x480x60 with three consumers, one delayed; p50/p95/p99 RPC latency, router CPU, copy and readback cost separately, RSS, store live and peak bytes, outstanding roots, collection lag, queue lengths, for one, two and four agents | conforms | `tests/perf.rs::frames_at_60hz_with_three_consumers` (`--ignored`); numbers below |
| The first executable example: a counter RPC, a pub/sub observer and a frame artifact held past message consumption, in one small Rust program, no game or browser | conforms | `examples/demo.rs` (`cargo run -p flybus --example demo`), asserted by `tests/example_demo.rs::the_example_shows_a_counter_rpc_an_observer_and_a_held_frame` |

## The implementation guide's acceptance bullets

Every bullet of BUS-01, BUS-02 and BUS-03, with the named test that proves it. A bullet a
pre-existing suite already covered is cited here rather than duplicated; the rest are the
tests in `tests/bus_acceptance.rs`, named after their bullet.

### BUS-01 — router and RPC, in-memory and Unix socket parity

| Bullet | Test |
| --- | --- |
| Partial frames and writes | `tests/conformance_wire.rs::{a_frame_written_one_byte_at_a_time_still_decodes, a_reply_read_one_byte_at_a_time_still_decodes, truncated_frame_disconnects_cleanly, length_prefix_is_little_endian}` (both), `tests/sol_rereview_regressions.rs::{shutdown_cancels_partial_rpc_request_before_reclaiming_attachment, protocol_close_cancels_partial_topic_frame_before_reclaiming_attachment}` |
| Disconnect after request | `tests/rpc.rs::service_disconnect_fails_calls` (both), `tests/conformance_routing.rs::caller_disconnect_detaches_dispatched_call_but_service_keeps_serving` (both), `tests/sol_review_races.rs::caller_disconnect_cleanup_works_before_and_after_consumption` |
| Lost result | **new** `tests/bus_acceptance.rs::a_lost_result_leaks_no_roots_and_the_endpoint_cache_still_replays` (both) |
| Retransmission fixtures | **new** `tests/bus_acceptance.rs::a_retransmission_repeats_the_domain_request_under_a_fresh_call_id` (both), with `tests/rpc.rs::endpoint_cache_replays_artifact_results` (both) |
| No automatic retry | **new** `tests/bus_acceptance.rs::no_automatic_retry_or_failover_onto_a_replacement_registration` (both) |
| Cancel after dispatch reports execution-unknown | `tests/rpc.rs::cancellation_states` (both), `tests/sol_review_races.rs::reply_racing_cancel_has_only_the_two_contract_outcomes` |
| Incarnation replacement is visible | `tests/rpc.rs::registration_is_exclusive_and_pinned` (both), `tests/conformance_routing.rs::{duplicate_registration_by_owner_itself_is_rejected, unpinned_call_after_incarnation_replacement_reaches_the_new_holder}` (both) |
| Out-of-order replies | `tests/rpc.rs::fifo_dispatch_and_out_of_order_completion` (both), `tests/conformance_routing.rs::out_of_order_replies_correlate_across_concurrent_callers` (both) |
| Status RPC responds while another handler is delayed | **new** `tests/bus_acceptance.rs::a_status_rpc_responds_while_another_handler_is_delayed` (both) |
| Saturation is bounded | `tests/rpc.rs::{service_queue_backpressure, active_call_limit}` (both), `tests/wire.rs::control_lane_exhaustion_closes` (both), `tests/sol_review_races.rs::pending_connections_are_bounded_and_hello_expires` |
| Both transports produce equivalent behaviour traces | **new** `tests/bus_acceptance.rs::both_transports_produce_equivalent_behaviour_traces`, over the trace recorder in `tests/common/mod.rs::Trace` |

The recorder keeps behaviour and refuses operational identity: `Trace::record` panics on any
router-issued id, so a trace holds methods, payload fields, counts, sequences, credits, cancel
states and error codes only. The two scenarios (an RPC one and a pub/sub-plus-artifact one)
produce 29 events, identical over both transports; `FLYBUS_TRACE=1` prints them.

### BUS-02 — pub/sub, retention and backpressure

| Bullet | Test |
| --- | --- |
| Overflow rejects a bounded publication before partial fan-out | `tests/pubsub.rs::bounded_fifo_and_atomic_backpressure` (both), `tests/conformance_routing.rs::bounded_overflow_rolls_back_all_artifact_roots` (both) |
| Latest replaces only queued messages | `tests/pubsub.rs::latest_coalesces_only_undelivered_values` (both), `tests/conformance_artifacts.rs::latest_mode_holds_at_most_two_roots_delivered_plus_queued` (both) |
| Delivery consumption returns credits | `tests/pubsub.rs::credits_return_only_on_consume` (both), `tests/conformance_routing.rs::bounded_credit_waits_for_every_extracted_artifact` (both) |
| Unsubscribe preserves already-delivered ownership | `tests/pubsub.rs::unsubscribe_discards_queue_but_not_deliveries` (both), `tests/conformance_routing.rs::unsubscribe_cannot_reach_another_connections_subscription_id` (both) |
| Retained replay is ordered | `tests/pubsub.rs::retained_replay_clear_delete_and_incarnations` (both), `tests/conformance_routing.rs::{latest_replay_is_ordered_ahead_of_a_racing_publish, cleared_topic_gives_no_replay_until_a_fresh_publish}` (both), `tests/sol_review_races.rs::{retained_replay_obeys_bounded_queue_byte_quota, latest_replay_remains_bounded_outside_the_bounded_byte_pool}` |
| Stalled observers cannot starve RPC replies | `tests/pubsub.rs::saturated_subscriber_does_not_block_control` (both), `tests/sol_rereview_regressions.rs::{fair_topic_insertion_preserves_router_envelope_order, sdk_accepts_fair_topic_insertion_through_saturated_control_backlog}` |

All six were already covered, so BUS-02 added no test of its own. The equivalence trace above
carries a pub/sub and artifact scenario, so BUS-02's behaviour is in the transport comparison
too, and `a_latest_subscriber_never_refuses_a_publication` (both) proves the rule that sits
behind "latest replaces only queued messages": the replacement never turns into a refusal.

### BUS-03 — artifact-backed messages and automatic lifetimes

| Bullet | Test |
| --- | --- |
| Last owner collects | `tests/artifacts.rs::fan_out_shares_one_object_and_the_last_consumer_collects` (both), `tests/conformance_artifacts.rs::collection_waits_for_every_retained_owner` (both) |
| Extracted handle survives message drop | `tests/artifacts.rs::extracted_artifacts_outlive_their_message` (both), `tests/conformance_artifacts.rs::{extracted_artifact_outlives_the_message_it_came_from, explicit_retain_outlives_the_original_hold}` (both) |
| Forward before release is safe | `tests/conformance_artifacts.rs::forward_requires_the_source_owner_still_live` (both), `tests/sol_review_races.rs::unsent_oversized_call_rolls_back_its_local_slot`, `tests/integration.rs::session_over_one_router` (both; the coordinator forwards a frame to two agents) |
| Lost replies and cache replay remain valid | **new** `tests/bus_acceptance.rs::a_lost_result_leaks_no_roots_and_the_endpoint_cache_still_replays` (both), with `tests/rpc.rs::endpoint_cache_replays_artifact_results` (both) |
| Disconnect releases logical ownership without mutating still-mapped bytes | **new** `tests/bus_acceptance.rs::disconnect_releases_logical_ownership_without_mutating_open_bytes` (both), with `tests/artifacts.rs::{disconnect_releases_all_but_retained, seal_is_immune_to_live_writable_handles}` (both) |
| Retained latest and queue replacement release the correct roots | `tests/conformance_artifacts.rs::{latest_mode_holds_at_most_two_roots_delivered_plus_queued, retained_topic_value_holds_a_root_independent_of_subscribers, queued_deliveries_hold_roots_before_dispatch}` (both), `tests/conformance_routing.rs::subscriber_disconnect_releases_queued_and_delivered_artifacts_but_not_retention` (both) |
| Measure 640x480 RGBA x 60 with three readers: one stored image, no raw pixels in router messages, bounded CPU/RSS/owners/queues, reader and copy costs recorded | `tests/perf.rs::frames_at_60hz_with_three_consumers`; see the measurement below |

## The crate README's differences from the draft

Each of the ten differences the crate lists, kept with the sentence that allows it or fixed.

| README difference | Verdict |
| --- | --- |
| 1. Extra error codes `CONFLICT`, `NO_TOPIC`, `ARTIFACT_MISMATCH` | Kept. Allowed by section 9: "Transport errors **include** `INVALID_ENVELOPE`, ..." — an inclusive list. Each names a refusal the draft requires without naming its code, so all three are now in the bus-v1 amendment (section 12) |
| 2. Topics must be declared; `topic.clear` of an unknown topic is `NO_TOPIC` while `topic.delete` answers `deleted:false` | Kept. The draft is silent; section 7's "Topic count and retained bytes are capped" and `topic.declare`'s "conflicting settings fail" both presuppose a registry. See the section 7 row |
| 3. When notices are sent | Kept. Section 5 requires the three notices but says nothing about audience or timing: "Required bounded notices are route removal, subscription closure and call failure" |
| 4. Byte budgets: `max_queued_bytes_per_client` counts bounded subscriptions only; `max_retained_bytes` added | Kept, and no longer a difference: the section 9 table now names the bounded pool and bounds latest slots separately (amendment, contradiction 1), and section 7's "Topic count and retained bytes are capped" requires the retained cap |
| 5. Admission sizes a delivery with the router-added ids at their longest, so an inbound envelope near 65,536 bytes can be refused although it fits | Kept, and required: section 5's "Message length includes this wrapper" plus section 4's fixed maximum mean the delivery the router would build must also fit. ipc-v1 section 2's "must not silently truncate a payload" forbids the alternative |
| 6. One live connection per client id; a client id's last incarnation may not be reused; only `*_as` endpoints authenticate the Hello id | Kept. See the section 3 and 4 rows: section 3's per-incarnation callId uniqueness and section 4's launcher-provided privileges |
| 7. The seal reply's ownerId is the writer's own id, now a hold | Kept. Section 8.2: "seal transfers its unique writer" — one owner token, transferred |
| 8. No `budget` argument on calls, and no router executable | Kept. Section 1 calls the executable "optional"; section 6 puts the deadline in the calling client, and the section 2 sketch no longer shows a budget either (amendment, contradiction 2) |
| 9. Wire strictness: unknown fields in management bodies refused, `minor` must be 0 after hello | Kept. Stricter than section 4's "unknown envelope fields", forbidden nowhere, and section 4's envelope literally fixes `minor: 0` |
| 10. `rpc.responder.release` and its terminal `call.failed` | Kept. Section 4 allows draft schema changes ("Changes to these draft schemas change contractDigest"), and it is how section 6's "bounded call correlation metadata" stays bounded when a handler outlives its request delivery. Added to the bus-v1 amendment (section 12) |

## Measured on the dev VM

Two complete release runs of `cargo test --release -p flybus --test perf -- --ignored
--nocapture` on the development VM (4 CPUs), 640x480 RGBA at 60 Hz for 2 s per schedule, three
latest-mode consumers with the third delayed 40 ms per frame, and one, two and four agent
services called every frame. The router runs on its own two-thread Tokio runtime whose threads
carry a distinct name, so its CPU (routing plus the seal copies on its blocking pool) is
measured apart from the clients' four threads in the same process. Each cell is the range over
the two runs.

| Metric | 1 agent | 2 agents | 4 agents |
| --- | --- | --- | --- |
| Frames produced (late) | 120 (0) | 120 (0) | 120 (0 and 3) |
| RPC round trip ms p50/p95/p99 | 0.51-0.60 / 0.84-1.47 / 1.12-2.05 | 0.76-0.78 / 1.34-2.21 / 2.14-6.18 | 1.04-1.08 / 1.67-5.01 / 2.06-10.02 |
| allocate (quota + staging file) ms | 0.96-1.01 / 1.13-1.28 / 1.34-1.49 | 0.98-1.02 / 1.22-3.50 / 1.82-5.61 | 0.80-0.94 / 1.15-5.32 / 1.31-6.60 |
| producer copy into staging ms | 0.37-0.38 / 0.45-0.49 / 0.67-0.75 | 0.37 / 0.45 / 0.51-0.55 | 0.36-0.39 / 0.41-0.49 / 0.48-0.77 |
| seal, router copy to a sealed inode, ms | 1.31-1.35 / 1.65-1.73 / 1.89-1.90 | 1.27-1.42 / 1.69-4.97 / 1.87-7.41 | 1.14-1.30 / 1.64-5.33 / 1.72-7.00 |
| publish admission ms | 0.41-0.42 / 0.61-0.85 / 0.68-1.11 | 0.41-0.43 / 0.83-1.07 / 1.17-3.10 | 0.42-0.49 / 0.73-2.75 / 1.52-4.54 |
| consumer readback of 1.2 MB ms | 0.65-0.75 / 1.19-1.53 / 1.45-1.92 | 0.76-0.91 / 1.45-2.20 / 1.76-6.02 | 0.89-0.99 / 1.67-4.85 / 2.04-6.38 |
| Router CPU (cores, of 2 threads) | 0.175-0.185 | 0.200 | 0.175-0.215 |
| Whole process CPU (cores, of 6 threads) | 0.325-0.335 | 0.410-0.425 | 0.370-0.460 |
| VmRSS / VmHWM | 11.2-13.6 / 13.6-14.4 MB | 11.8-14.1 / 15.0 MB | 15.7-15.8 / 16.4-16.6 MB |
| Store bytes peak / live after drain | 3.7 MB / 0 | 3.7 MB / 0 | 3.7 MB / 0 |
| Outstanding roots peak / live | 5-6 / 0 | 6 / 0 | 5-6 / 0 |
| Queue length peak / live | 1 / 0 | 1 / 0 | 1 / 0 |
| Collection lag after the last frame | 63.5-74.7 ms | 73.6-83.0 ms | 44.1-68.2 ms |
| Frames seen by the three consumers (coalesced) | 120, 120, 49 (0, 0, 70-71) | 120, 120, 49 (0, 0, 71) | 120, 120, 47-49 (0, 0, 70-71) |

What the numbers do and do not say:

- **These are not capacity claims.** Two runs of a synthetic two-second schedule, in one
  process, on one shared virtual machine with fewer CPUs than the process has threads, over
  temporary local storage, with no capture device, encoder or renderer in the path. They are a
  floor on cost, not a ceiling on throughput, and nothing here licenses a sizing decision.
- The second run's tails are three to five times the first run's (RPC p99 10.02 ms against
  2.06 ms at four agents, three late frames against none) because other work shared the VM's
  four CPUs during it. The p50s barely moved. That spread is the honest width of a measurement
  on a shared host, not a property of the bus.
- No byte fan-out: three consumers of a 1.2 MB frame kept the store's peak at 3.7 MB, which is
  one sealed object plus the staging and sealing copies of the next frame, not one object per
  consumer. Outstanding roots peaked at six.
- Copy costs are real and dominate routing: the producer's copy into staging (p50 0.4 ms), the
  router's seal copy into a fresh inode (p50 1.1 to 1.4 ms) and a consumer's readback (p50 0.7
  to 1.0 ms) each cost as much as or more than publish admission (p50 0.4 ms).
- The delayed consumer coalesced 70 or 71 of 120 frames and never caused a rejected
  publication, which is section 9's rule about latest spectators in one number.
- Every schedule drained completely: live store bytes, roots and queue lengths are zero
  afterwards, 44 to 83 ms behind the last frame.
- Router CPU grows with agent count much more slowly than the whole process (0.18 to 0.22
  cores against 0.33 to 0.46), because the clients own the copies. A thread that exits between
  two samples takes its CPU with it, so the router figure is a floor.

## Contradictions

Two, both inside bus-v1, both minor, neither resolved by changing code. Both were referred to
the coordinator rather than guessed at, and both now carry a dated amendment in bus-v1
section 12; the rows above cite the amended wording. The original reading is kept here because
it is the reason for the amendment.

1. **The per-client queued envelope byte budget versus the latest-mode guarantee.** Section 9's
   table has "Per-client ordinary queued envelope bytes | 1 MiB". Section 7 requires that a
   latest subscriber always has one replaceable queued value, and section 9 itself says "latest
   spectator subscriptions cannot hold a required session transaction indefinitely" — that is, a
   latest subscriber must never be the reason a publication is rejected. A byte budget that
   covered latest slots and rejected on overflow would violate the second statement; a budget
   that excludes them is not the sentence in the table. The crate excludes them, which keeps the
   normative sentence and loosens the table row: the worst case becomes subscriptions x 64 KiB
   (8 MiB at the default 128 subscriptions per client) instead of 1 MiB. **Resolved in the
   spec, not the code** (2026-09-22): the table row now names the bounded pool and latest slots
   have their own row, so the implementation conforms as written. No behaviour changed, and
   `a_latest_subscriber_never_refuses_a_publication` now proves the guarantee directly.
2. **A call `budget` versus a client-owned deadline.** Section 2's illustrative surface passes a
   `budget` into `bus.call(...)`, while section 6 states "A deadline belongs to the calling
   client" and gives the router no timeout behaviour, and section 5's `rpc.call` body has no
   budget field. The crate follows sections 5 and 6 and has no budget argument, which is safe
   because section 2 is labelled illustrative. **Resolved in the spec, not the code**
   (2026-09-22): the sketch drops `budget` and shows the deadline at the caller, so the
   illustrative surface and the wire contract now agree.

Ambiguities resolved without treating them as contradictions, for the record:

- Section 5's "Reply only by the registered recipient" is read as "by the connection the request
  was delivered to", so that section 6's "dispatched calls can still be answered" after
  `service.unregister` remains possible. The crate correlates replies by request delivery id on
  that connection, not by the live registration.
- Section 9's "Active calls per client 64" is read as calls the caller still awaits: a call
  detached by a post-dispatch cancel frees its caller slot while the router keeps the bounded
  correlation record until the service consumes or answers it.

## Not implemented, and who owns it

- A standalone router executable (section 1 calls it optional): embed `Router`.
- Bindings in other languages (section 1: a binding "may" exist), and the mapping/FFI pointer
  lifetime rules that section 8.3 writes for them.
- A memory storage backend (section 4) and pooled shared memory with generation reuse
  (section 8.4), both deferred by the draft.
- Domain-schema enforcement that every referenced artifact is listed in attachments (section 4),
  and the domain `RESULT_EXPIRED` outcome (section 6): CONTRACT-01 and `fly-session-rpc`.
- Epoch failure and coherent recovery after a router restart (section 8.4): the session
  contract, STATE-01.
- Any consumer at all: no other crate depends on flybus yet, so the feed and control surfaces of
  [feed-protocol](../../feed-protocol.md) and [control-api](../../control-api.md) are untouched
  and the crate's "Wiring still pending" list still stands.
