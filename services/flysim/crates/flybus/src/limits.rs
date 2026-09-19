//! Router limits (bus-v1 section 9). The defaults are the draft's prototype starting point,
//! not capacity data.

use serde_json::{Map, Value};

use crate::wire::{Fields, MAX_ATTACHMENTS, MAX_BATCH, MAX_CREDIT, MAX_ENVELOPE_BYTES, WireError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_clients: usize,
    pub max_services: usize,
    pub max_topics: usize,
    pub max_subscriptions_per_client: usize,
    pub max_subscriptions: usize,
    /// Calls a client may have admitted and not yet finished (result consumed, failed or
    /// cancelled before dispatch). A call detached by a post-dispatch cancel no longer counts.
    pub max_active_calls_per_client: usize,
    /// Upper bounds on what `service.register` may ask for.
    pub max_service_queued: u64,
    pub max_service_in_flight: u64,
    /// Upper bound on a `latest` subscription's credits; its queue is always exactly 1.
    pub max_latest_in_flight: u64,
    pub max_bounded_queued: u64,
    pub max_bounded_in_flight: u64,
    /// Owners (deliveries handed to the client and not consumed, explicit holds and staging
    /// writers) a connection may have at once.
    pub max_owners_per_client: usize,
    /// Of those, how many only RPC request/result deliveries may use. Topic deliveries, holds
    /// and writers are refused (holds, writers) or wait (deliveries) beyond
    /// `max_owners_per_client - reserved_owners_per_client`.
    pub reserved_owners_per_client: usize,
    /// Bytes of staging, sealing copies and sealed objects the store may hold.
    pub max_store_bytes: u64,
    pub max_artifact_bytes: u64,
    /// Artifact bytes pinned by retained topic values, counted once per topic.
    pub max_retained_bytes: u64,
    /// Envelope bytes queued for a client's `bounded` subscriptions and not yet delivered.
    pub max_queued_bytes_per_client: usize,
    /// Replies and notices waiting to be written to one client. Exceeding either closes it.
    pub max_control_frames: usize,
    pub max_control_bytes: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_clients: 64,
            max_services: 256,
            max_topics: 512,
            max_subscriptions_per_client: 128,
            max_subscriptions: 1024,
            max_active_calls_per_client: 64,
            max_service_queued: 16,
            max_service_in_flight: 16,
            max_latest_in_flight: 2,
            max_bounded_queued: 64,
            max_bounded_in_flight: 16,
            max_owners_per_client: 256,
            reserved_owners_per_client: 64,
            max_store_bytes: 512 << 20,
            max_artifact_bytes: 128 << 20,
            max_retained_bytes: 128 << 20,
            max_queued_bytes_per_client: 1 << 20,
            max_control_frames: 128,
            max_control_bytes: 1 << 20,
        }
    }
}

impl Limits {
    /// Refuses a configuration the router could not honour.
    pub fn validate(&self) -> Result<(), String> {
        let credits = [
            ("max_service_queued", self.max_service_queued),
            ("max_service_in_flight", self.max_service_in_flight),
            ("max_latest_in_flight", self.max_latest_in_flight),
            ("max_bounded_queued", self.max_bounded_queued),
            ("max_bounded_in_flight", self.max_bounded_in_flight),
        ];
        for (name, v) in credits {
            if !(1..=MAX_CREDIT).contains(&v) {
                return Err(format!("{name} must be in 1..={MAX_CREDIT}"));
            }
        }
        if self.reserved_owners_per_client >= self.max_owners_per_client {
            return Err("reserved_owners_per_client must be below max_owners_per_client".into());
        }
        if self.max_artifact_bytes > self.max_store_bytes {
            return Err("max_artifact_bytes must not exceed max_store_bytes".into());
        }
        if self.max_control_frames == 0 || self.max_control_bytes < MAX_ENVELOPE_BYTES {
            return Err("the control lane must hold at least one full envelope".into());
        }
        Ok(())
    }

    /// The `limits` object in the hello reply. Counts are JSON integers; byte sizes are U64
    /// strings.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        let mut n = |k: &str, v: u64| {
            m.insert(k.into(), Value::from(v));
        };
        n("maxEnvelopeBytes", MAX_ENVELOPE_BYTES as u64);
        n("maxAttachments", MAX_ATTACHMENTS as u64);
        n("maxBatch", MAX_BATCH as u64);
        n("maxClients", self.max_clients as u64);
        n("maxServices", self.max_services as u64);
        n("maxTopics", self.max_topics as u64);
        n(
            "maxSubscriptionsPerClient",
            self.max_subscriptions_per_client as u64,
        );
        n("maxSubscriptions", self.max_subscriptions as u64);
        n(
            "maxActiveCallsPerClient",
            self.max_active_calls_per_client as u64,
        );
        n("maxServiceQueued", self.max_service_queued);
        n("maxServiceInFlight", self.max_service_in_flight);
        n("maxLatestInFlight", self.max_latest_in_flight);
        n("maxBoundedQueued", self.max_bounded_queued);
        n("maxBoundedInFlight", self.max_bounded_in_flight);
        n("maxOwnersPerClient", self.max_owners_per_client as u64);
        n(
            "reservedOwnersPerClient",
            self.reserved_owners_per_client as u64,
        );
        n("maxControlFrames", self.max_control_frames as u64);
        let mut s = |k: &str, v: u64| {
            m.insert(k.into(), Value::from(v.to_string()));
        };
        s("maxStoreBytes", self.max_store_bytes);
        s("maxArtifactBytes", self.max_artifact_bytes);
        s("maxRetainedBytes", self.max_retained_bytes);
        s(
            "maxQueuedBytesPerClient",
            self.max_queued_bytes_per_client as u64,
        );
        s("maxControlBytes", self.max_control_bytes as u64);
        Value::Object(m)
    }

    /// Reads the hello reply's `limits` object back.
    pub fn from_json(v: &Value) -> Result<Limits, WireError> {
        let mut f = Fields::new(v, "limits")?;
        let big = u64::MAX;
        for (k, want) in [
            ("maxEnvelopeBytes", MAX_ENVELOPE_BYTES as u64),
            ("maxAttachments", MAX_ATTACHMENTS as u64),
            ("maxBatch", MAX_BATCH as u64),
        ] {
            if f.int(k, 0, big)? != want {
                return Err(WireError(format!(
                    "limits: {k} disagrees with this implementation"
                )));
            }
        }
        let limits = Limits {
            max_clients: f.int("maxClients", 0, big)? as usize,
            max_services: f.int("maxServices", 0, big)? as usize,
            max_topics: f.int("maxTopics", 0, big)? as usize,
            max_subscriptions_per_client: f.int("maxSubscriptionsPerClient", 0, big)? as usize,
            max_subscriptions: f.int("maxSubscriptions", 0, big)? as usize,
            max_active_calls_per_client: f.int("maxActiveCallsPerClient", 0, big)? as usize,
            max_service_queued: f.int("maxServiceQueued", 1, MAX_CREDIT)?,
            max_service_in_flight: f.int("maxServiceInFlight", 1, MAX_CREDIT)?,
            max_latest_in_flight: f.int("maxLatestInFlight", 1, MAX_CREDIT)?,
            max_bounded_queued: f.int("maxBoundedQueued", 1, MAX_CREDIT)?,
            max_bounded_in_flight: f.int("maxBoundedInFlight", 1, MAX_CREDIT)?,
            max_owners_per_client: f.int("maxOwnersPerClient", 0, big)? as usize,
            reserved_owners_per_client: f.int("reservedOwnersPerClient", 0, big)? as usize,
            max_control_frames: f.int("maxControlFrames", 0, big)? as usize,
            max_store_bytes: f.u64_string("maxStoreBytes")?,
            max_artifact_bytes: f.u64_string("maxArtifactBytes")?,
            max_retained_bytes: f.u64_string("maxRetainedBytes")?,
            max_queued_bytes_per_client: f.u64_string("maxQueuedBytesPerClient")? as usize,
            max_control_bytes: f.u64_string("maxControlBytes")? as usize,
        };
        f.finish()?;
        Ok(limits)
    }
}
