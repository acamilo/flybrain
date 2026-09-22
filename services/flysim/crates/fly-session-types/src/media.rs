//! Native observation media (state-media-v1 section 2) and the State.* payloads (section 5).
//!
//! Descriptors carry the shape; refs carry one produced object. Both are validated against
//! the descriptor, because a ref on its own cannot know its own row stride: use
//! [`ViewRef::validate_against`] and [`AudioRef::validate_against`] wherever the descriptor
//! is in hand.

use flybus::wire::{ArtifactRef, Fields};

use serde_json::Value;

use crate::scalar::{
    DomainType, RationalNs, Result, Scope, constant, err, finite_in, is_digest, is_id, list, obj,
    require_unique, u64_json,
};

/// Max views per sensory input (workers-v1 section 1). The same bound applies to a
/// descriptor's view list and to an observation's view lists: a descriptor that declared more
/// views than one sensory input can carry could not be satisfied.
pub const MAX_VIEWS: usize = 8;
/// View dimensions are integers 1..=4096 (state-media-v1 section 2).
pub const MAX_VIEW_DIMENSION: u64 = 4096;
/// Pixel aspect numerator/denominator are positive integers <=65535.
pub const MAX_PIXEL_ASPECT: u64 = 65_535;
/// observationDelaySteps is an integer 0..=8.
pub const MAX_OBSERVATION_DELAY_STEPS: u64 = 8;
/// sampleFrames is 0..=192000 per chunk; sampleRate is 8000..=192000.
pub const MAX_SAMPLE_FRAMES: u64 = 192_000;
/// Audio streams per descriptor. Not a stated bound: chosen so an envelope cannot be filled
/// with descriptors, and recorded in the schema set so it cannot drift silently.
pub const MAX_AUDIO_STREAMS: usize = 8;

/// `ViewDescriptor`: the fixed shape of one native view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewDescriptor {
    pub view_id: String,
    pub width: u64,
    pub height: u64,
    pub row_stride: u64,
    pub pixel_aspect_numerator: u64,
    pub pixel_aspect_denominator: u64,
    pub observation_delay_steps: u64,
}

impl ViewDescriptor {
    /// The exact byte length of one frame of this view.
    pub fn frame_bytes(&self) -> u64 {
        self.row_stride * self.height
    }

    /// The producing boundary a required sensory view must have at `boundary`
    /// (state-media-v1 section 2): `max(0, boundary - observationDelaySteps)`.
    pub fn required_produced_step(&self, boundary: u64) -> u64 {
        boundary.saturating_sub(self.observation_delay_steps)
    }
}

impl DomainType for ViewDescriptor {
    const TYPE_NAME: &'static str = "ViewDescriptor";

    fn from_json(value: &Value) -> Result<ViewDescriptor> {
        let mut f = Fields::new(value, "ViewDescriptor")?;
        let view_id = f.id("viewId")?;
        let width = f.int("width", 1, MAX_VIEW_DIMENSION)?;
        let height = f.int("height", 1, MAX_VIEW_DIMENSION)?;
        constant(&mut f, "format", "rgba8")?;
        let row_stride = f.int("rowStride", 1, MAX_VIEW_DIMENSION * 4)?;
        let aspect = f.value("pixelAspect")?;
        let (pixel_aspect_numerator, pixel_aspect_denominator) = {
            let mut a = Fields::new(aspect, "ViewDescriptor.pixelAspect")?;
            let n = a.int("numerator", 1, MAX_PIXEL_ASPECT)?;
            let d = a.int("denominator", 1, MAX_PIXEL_ASPECT)?;
            a.finish()?;
            (n, d)
        };
        let observation_delay_steps =
            f.int("observationDelaySteps", 0, MAX_OBSERVATION_DELAY_STEPS)?;
        f.finish()?;
        let d = ViewDescriptor {
            view_id,
            width,
            height,
            row_stride,
            pixel_aspect_numerator,
            pixel_aspect_denominator,
            observation_delay_steps,
        };
        d.validate()?;
        Ok(d)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("viewId", self.view_id.clone().into()),
            ("width", Value::from(self.width)),
            ("height", Value::from(self.height)),
            ("format", "rgba8".into()),
            ("rowStride", Value::from(self.row_stride)),
            (
                "pixelAspect",
                obj(vec![
                    ("numerator", Value::from(self.pixel_aspect_numerator)),
                    ("denominator", Value::from(self.pixel_aspect_denominator)),
                ]),
            ),
            (
                "observationDelaySteps",
                Value::from(self.observation_delay_steps),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.view_id) {
            return err("ViewDescriptor: viewId is not a valid id");
        }
        if !(1..=MAX_VIEW_DIMENSION).contains(&self.width)
            || !(1..=MAX_VIEW_DIMENSION).contains(&self.height)
        {
            return err("ViewDescriptor: width and height must be integers 1..=4096");
        }
        if self.row_stride != self.width * 4 {
            return err(
                "ViewDescriptor: rowStride must be exactly 4 x width (no padded rows in v1)",
            );
        }
        if !(1..=MAX_PIXEL_ASPECT).contains(&self.pixel_aspect_numerator)
            || !(1..=MAX_PIXEL_ASPECT).contains(&self.pixel_aspect_denominator)
        {
            return err("ViewDescriptor: pixelAspect parts must be positive integers <=65535");
        }
        if self.observation_delay_steps > MAX_OBSERVATION_DELAY_STEPS {
            return err("ViewDescriptor: observationDelaySteps must be 0..=8");
        }
        Ok(())
    }
}

/// `ViewRef`: one produced frame of one view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewRef {
    pub view_id: String,
    pub produced_step: u64,
    pub pixels: ArtifactRef,
}

impl ViewRef {
    /// Byte shape and producing boundary against the descriptor that declared this view.
    ///
    /// `boundary` is the observation's boundary; a required sensory view must have been
    /// produced at exactly `max(0, boundary - observationDelaySteps)`.
    pub fn validate_against(
        &self,
        descriptor: &ViewDescriptor,
        boundary: Option<u64>,
    ) -> Result<()> {
        if self.view_id != descriptor.view_id {
            return err(format!(
                "ViewRef: viewId {:?} does not match descriptor {:?}",
                self.view_id, descriptor.view_id
            ));
        }
        if self.pixels.byte_length != descriptor.frame_bytes() {
            return err(format!(
                "ViewRef {}: artifact is {} bytes, rowStride x height is {}",
                self.view_id,
                self.pixels.byte_length,
                descriptor.frame_bytes()
            ));
        }
        if let Some(boundary) = boundary {
            let expected = descriptor.required_produced_step(boundary);
            if self.produced_step != expected {
                return err(format!(
                    "ViewRef {}: producedStep {} must be max(0, {boundary} - {}) = {expected}",
                    self.view_id, self.produced_step, descriptor.observation_delay_steps
                ));
            }
        }
        Ok(())
    }
}

impl DomainType for ViewRef {
    const TYPE_NAME: &'static str = "ViewRef";

    fn from_json(value: &Value) -> Result<ViewRef> {
        let mut f = Fields::new(value, "ViewRef")?;
        let view_id = f.id("viewId")?;
        let produced_step = f.u64_string("producedStep")?;
        let pixels = ArtifactRef::from_json(f.value("pixels")?)?;
        f.finish()?;
        let r = ViewRef {
            view_id,
            produced_step,
            pixels,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("viewId", self.view_id.clone().into()),
            ("producedStep", u64_json(self.produced_step)),
            ("pixels", self.pixels.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.view_id) {
            return err("ViewRef: viewId is not a valid id");
        }
        if self.pixels.byte_length == 0 {
            return err("ViewRef: pixels must have a positive byte length");
        }
        Ok(())
    }
}

/// `AudioDescriptor`: one native audio stream's shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDescriptor {
    pub stream_id: String,
    pub sample_rate: u64,
    pub channels: u64,
}

impl AudioDescriptor {
    /// The exact byte length of `frames` interleaved f32 frames.
    pub fn chunk_bytes(&self, frames: u64) -> u64 {
        frames * self.channels * 4
    }
}

impl DomainType for AudioDescriptor {
    const TYPE_NAME: &'static str = "AudioDescriptor";

    fn from_json(value: &Value) -> Result<AudioDescriptor> {
        let mut f = Fields::new(value, "AudioDescriptor")?;
        let stream_id = f.id("streamId")?;
        let sample_rate = f.int("sampleRate", 8_000, 192_000)?;
        let channels = f.int("channels", 1, 8)?;
        constant(&mut f, "format", "f32le-interleaved")?;
        f.finish()?;
        let d = AudioDescriptor {
            stream_id,
            sample_rate,
            channels,
        };
        d.validate()?;
        Ok(d)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("streamId", self.stream_id.clone().into()),
            ("sampleRate", Value::from(self.sample_rate)),
            ("channels", Value::from(self.channels)),
            ("format", "f32le-interleaved".into()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.stream_id) {
            return err("AudioDescriptor: streamId is not a valid id");
        }
        if !(8_000..=192_000).contains(&self.sample_rate) {
            return err("AudioDescriptor: sampleRate must be an integer 8000..=192000");
        }
        if !(1..=8).contains(&self.channels) {
            return err("AudioDescriptor: channels must be an integer 1..=8");
        }
        Ok(())
    }
}

/// `AudioRef`: one produced chunk of one audio stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioRef {
    pub stream_id: String,
    pub first_sample: u64,
    pub sample_frames: u64,
    pub samples: ArtifactRef,
    pub discontinuity: bool,
}

impl AudioRef {
    /// Byte shape against the descriptor that declared this stream.
    pub fn validate_against(&self, descriptor: &AudioDescriptor) -> Result<()> {
        if self.stream_id != descriptor.stream_id {
            return err(format!(
                "AudioRef: streamId {:?} does not match descriptor {:?}",
                self.stream_id, descriptor.stream_id
            ));
        }
        let expected = descriptor.chunk_bytes(self.sample_frames);
        if self.samples.byte_length != expected {
            return err(format!(
                "AudioRef {}: artifact is {} bytes, sampleFrames x channels x 4 is {expected}",
                self.stream_id, self.samples.byte_length
            ));
        }
        Ok(())
    }

    /// Within an epoch chunks cannot overlap or go backwards (state-media-v1 section 2).
    pub fn follows(&self, previous: &AudioRef) -> Result<()> {
        if self.stream_id != previous.stream_id {
            return err("AudioRef: chunks of different streams are not ordered against each other");
        }
        let expected = previous.first_sample + previous.sample_frames;
        if self.first_sample < expected {
            return err(format!(
                "AudioRef {}: firstSample {} overlaps the previous chunk, which ends at {expected}",
                self.stream_id, self.first_sample
            ));
        }
        Ok(())
    }
}

impl DomainType for AudioRef {
    const TYPE_NAME: &'static str = "AudioRef";

    fn from_json(value: &Value) -> Result<AudioRef> {
        let mut f = Fields::new(value, "AudioRef")?;
        let stream_id = f.id("streamId")?;
        let first_sample = f.u64_string("firstSample")?;
        let sample_frames = f.int("sampleFrames", 0, MAX_SAMPLE_FRAMES)?;
        let samples = ArtifactRef::from_json(f.value("samples")?)?;
        let discontinuity = f.boolean("discontinuity")?;
        f.finish()?;
        let r = AudioRef {
            stream_id,
            first_sample,
            sample_frames,
            samples,
            discontinuity,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("streamId", self.stream_id.clone().into()),
            ("firstSample", u64_json(self.first_sample)),
            ("sampleFrames", Value::from(self.sample_frames)),
            ("samples", self.samples.to_json()),
            ("discontinuity", Value::Bool(self.discontinuity)),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.stream_id) {
            return err("AudioRef: streamId is not a valid id");
        }
        if self.sample_frames > MAX_SAMPLE_FRAMES {
            return err("AudioRef: sampleFrames must be an integer 0..=192000");
        }
        if self.first_sample.checked_add(self.sample_frames).is_none() {
            return err("AudioRef: firstSample + sampleFrames overflows U64");
        }
        Ok(())
    }
}

/// Reads a bounded, unique-by-`viewId` list of view refs.
pub fn view_list(f: &mut Fields<'_>, key: &'static str) -> Result<Vec<ViewRef>> {
    let views = list(f, key, 0, MAX_VIEWS, ViewRef::from_json)?;
    require_unique(views.iter().map(|v| v.view_id.as_str()), key)?;
    Ok(views)
}

/// Reads a bounded, unique-by-`streamId` list of audio refs.
pub fn audio_list(f: &mut Fields<'_>, key: &'static str) -> Result<Vec<AudioRef>> {
    let audio = list(f, key, 0, MAX_AUDIO_STREAMS, AudioRef::from_json)?;
    require_unique(audio.iter().map(|a| a.stream_id.as_str()), key)?;
    Ok(audio)
}

// ---------------------------------------------------------------------------------------------
// State.* payloads (state-media-v1 section 5)

/// A checkpoint payload artifact: the digest is mandatory on checkpoint payloads
/// (state-media-v1 section 1).
fn checkpoint_payload(f: &mut Fields<'_>, key: &'static str) -> Result<ArtifactRef> {
    let reference = ArtifactRef::from_json(f.value(key)?)?;
    match &reference.digest {
        Some(d) if is_digest(d) => Ok(reference),
        _ => err(format!(
            "{key}: a checkpoint payload must carry a content digest"
        )),
    }
}

/// `State.Capture` params.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureParams {
    pub checkpoint_id: String,
}

impl DomainType for CaptureParams {
    const TYPE_NAME: &'static str = "CaptureParams";

    fn from_json(value: &Value) -> Result<CaptureParams> {
        let mut f = Fields::new(value, "CaptureParams")?;
        let checkpoint_id = f.id("checkpointId")?;
        f.finish()?;
        let p = CaptureParams { checkpoint_id };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![("checkpointId", self.checkpoint_id.clone().into())])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.checkpoint_id) {
            return err("CaptureParams: checkpointId is not a valid id");
        }
        Ok(())
    }
}

/// `State.Capture` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureResult {
    pub checkpoint_id: String,
    pub boundary: u64,
    pub compatibility_digest: String,
    pub payload: ArtifactRef,
}

impl DomainType for CaptureResult {
    const TYPE_NAME: &'static str = "CaptureResult";

    fn from_json(value: &Value) -> Result<CaptureResult> {
        let mut f = Fields::new(value, "CaptureResult")?;
        let checkpoint_id = f.id("checkpointId")?;
        let boundary = f.u64_string("boundary")?;
        let compatibility_digest = f.string("compatibilityDigest")?.to_owned();
        let payload = checkpoint_payload(&mut f, "payload")?;
        f.finish()?;
        let r = CaptureResult {
            checkpoint_id,
            boundary,
            compatibility_digest,
            payload,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("checkpointId", self.checkpoint_id.clone().into()),
            ("boundary", u64_json(self.boundary)),
            (
                "compatibilityDigest",
                self.compatibility_digest.clone().into(),
            ),
            ("payload", self.payload.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.checkpoint_id) {
            return err("CaptureResult: checkpointId is not a valid id");
        }
        if !is_digest(&self.compatibility_digest) {
            return err("CaptureResult: compatibilityDigest must be 64 lowercase hex digits");
        }
        match &self.payload.digest {
            Some(d) if is_digest(d) => Ok(()),
            _ => err("CaptureResult: a checkpoint payload must carry a content digest"),
        }
    }
}

/// `State.StageRestore` params. The scope is the source boundary, under a proposed new epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageRestoreParams {
    pub checkpoint_id: String,
    pub source_scope: Scope,
    pub compatibility_digest: String,
    pub payload: ArtifactRef,
}

impl DomainType for StageRestoreParams {
    const TYPE_NAME: &'static str = "StageRestoreParams";

    fn from_json(value: &Value) -> Result<StageRestoreParams> {
        let mut f = Fields::new(value, "StageRestoreParams")?;
        let checkpoint_id = f.id("checkpointId")?;
        let source_scope = Scope::from_json(f.value("sourceScope")?)?;
        let compatibility_digest = f.string("compatibilityDigest")?.to_owned();
        let payload = checkpoint_payload(&mut f, "payload")?;
        f.finish()?;
        let p = StageRestoreParams {
            checkpoint_id,
            source_scope,
            compatibility_digest,
            payload,
        };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("checkpointId", self.checkpoint_id.clone().into()),
            ("sourceScope", self.source_scope.to_json()),
            (
                "compatibilityDigest",
                self.compatibility_digest.clone().into(),
            ),
            ("payload", self.payload.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.checkpoint_id) {
            return err("StageRestoreParams: checkpointId is not a valid id");
        }
        self.source_scope.validate()?;
        if !is_digest(&self.compatibility_digest) {
            return err("StageRestoreParams: compatibilityDigest must be 64 lowercase hex digits");
        }
        match &self.payload.digest {
            Some(d) if is_digest(d) => Ok(()),
            _ => err("StageRestoreParams: a checkpoint payload must carry a content digest"),
        }
    }
}

/// `State.StageRestore` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageRestoreResult {
    pub checkpoint_id: String,
    pub restore_token: String,
}

impl DomainType for StageRestoreResult {
    const TYPE_NAME: &'static str = "StageRestoreResult";

    fn from_json(value: &Value) -> Result<StageRestoreResult> {
        let mut f = Fields::new(value, "StageRestoreResult")?;
        let checkpoint_id = f.id("checkpointId")?;
        let restore_token = f.id("restoreToken")?;
        f.finish()?;
        let r = StageRestoreResult {
            checkpoint_id,
            restore_token,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("checkpointId", self.checkpoint_id.clone().into()),
            ("restoreToken", self.restore_token.clone().into()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.checkpoint_id) || !is_id(&self.restore_token) {
            return err("StageRestoreResult: checkpointId and restoreToken must be valid ids");
        }
        Ok(())
    }
}

/// `State.ActivateRestore` params.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivateRestoreParams {
    pub restore_token: String,
}

impl DomainType for ActivateRestoreParams {
    const TYPE_NAME: &'static str = "ActivateRestoreParams";

    fn from_json(value: &Value) -> Result<ActivateRestoreParams> {
        let mut f = Fields::new(value, "ActivateRestoreParams")?;
        let restore_token = f.id("restoreToken")?;
        f.finish()?;
        let p = ActivateRestoreParams { restore_token };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![("restoreToken", self.restore_token.clone().into())])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.restore_token) {
            return err("ActivateRestoreParams: restoreToken is not a valid id");
        }
        Ok(())
    }
}

/// `State.ActivateRestore` result. The observation is required from an environment and null
/// from an agent (state-media-v1 section 5); which one applies is the caller's role, so the
/// role-specific check is [`ActivateRestoreResult::validate_for_role`].
#[derive(Clone, Debug, PartialEq)]
pub struct ActivateRestoreResult {
    pub committed_step: u64,
    pub checkpoint_id: String,
    pub observation: Option<crate::workers::WorldObservation>,
}

impl ActivateRestoreResult {
    pub fn validate_for_role(&self, role: crate::workers::Role) -> Result<()> {
        self.validate()?;
        match (role, &self.observation) {
            (crate::workers::Role::Environment, None) => {
                err("ActivateRestoreResult: an environment must return its restored observation")
            }
            (crate::workers::Role::Agent, Some(_)) => {
                err("ActivateRestoreResult: an agent returns a null observation")
            }
            _ => Ok(()),
        }
    }
}

impl DomainType for ActivateRestoreResult {
    const TYPE_NAME: &'static str = "ActivateRestoreResult";

    fn from_json(value: &Value) -> Result<ActivateRestoreResult> {
        let mut f = Fields::new(value, "ActivateRestoreResult")?;
        let committed_step = f.u64_string("committedStep")?;
        let checkpoint_id = f.id("checkpointId")?;
        let observation = match f.value("observation")? {
            Value::Null => None,
            v => Some(crate::workers::WorldObservation::from_json(v)?),
        };
        f.finish()?;
        let r = ActivateRestoreResult {
            committed_step,
            checkpoint_id,
            observation,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("committedStep", u64_json(self.committed_step)),
            ("checkpointId", self.checkpoint_id.clone().into()),
            (
                "observation",
                self.observation
                    .as_ref()
                    .map_or(Value::Null, |o| o.to_json()),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.checkpoint_id) {
            return err("ActivateRestoreResult: checkpointId is not a valid id");
        }
        if let Some(observation) = &self.observation {
            observation.validate()?;
            if observation.boundary != self.committed_step {
                return err(
                    "ActivateRestoreResult: the observation boundary must be the committed step",
                );
            }
        }
        Ok(())
    }
}

/// The pixel aspect of a view as a rational, for presentation.
pub fn pixel_aspect(descriptor: &ViewDescriptor) -> Result<RationalNs> {
    RationalNs::reduced(
        u128::from(descriptor.pixel_aspect_numerator),
        u128::from(descriptor.pixel_aspect_denominator),
    )
}

/// Audio presentation timestamp, `firstSample / sampleRate` seconds, as a checked rational.
pub fn audio_pts(reference: &AudioRef, descriptor: &AudioDescriptor) -> Result<RationalNs> {
    RationalNs::reduced(
        u128::from(reference.first_sample),
        u128::from(descriptor.sample_rate),
    )
}

/// Samples must be finite f32 (state-media-v1 section 2). The bytes live in an artifact, so
/// this is the check a reader runs over a mapped chunk.
pub fn require_finite_samples(bytes: &[u8]) -> Result<()> {
    if !bytes.len().is_multiple_of(4) {
        return err("audio chunk: length must be a multiple of 4");
    }
    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if !sample.is_finite() {
            return err(format!("audio chunk: sample {index} is not finite"));
        }
    }
    Ok(())
}

/// A unit-range helper for presentation code that needs the neutral-in-range rule.
pub fn require_unit(f: &mut Fields<'_>, key: &'static str) -> Result<f64> {
    finite_in(f, key, 0.0, 1.0)
}
