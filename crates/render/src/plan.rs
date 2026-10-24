//! Frame plans for the GPU compositor.
//!
//! [`plan_frame`] resolves what is visible at a time into a list of layers the GPU can draw
//! directly: a decoded source frame (YUV planes or RGBA), the matrix from source pixels to output
//! pixels, an opacity and a blend mode (all 27 of [`Blend`] are composited by the GPU). A media
//! clip whose enabled standard effects all have a GPU implementation ([`crate::gpufx`]) carries
//! them as a [`LayerFx`]: their parameters evaluated at the frame's time, run by the GPU on the
//! clip's working image before Motion places it, in the CPU's order (effects → Motion → Opacity /
//! blend). Anything the shaders don't cover yet — other standard effects, masks, adjustment
//! layers, nested sequences, transitions other than dissolves, dips and Push — is rendered on the CPU
//! for that layer (or the whole frame) and handed over as a pre-composited image, so the GPU path
//! is always exact with respect to the CPU reference.
//!
//! A Push transition is planned as ordered [`PlanStep`] operations on isolated clip inputs: each
//! side is drawn into its own image, the two are pushed together, and the result is composited (Normal) over
//! the canvas of the lower tracks, which is preserved for the blends above it. [`execute_cpu`]
//! runs the same steps on the CPU and is the reference.

use std::{collections::HashMap, sync::Arc};

use filmcraft_color::InputTransform;
use filmcraft_frame::VideoFrame;
use filmcraft_geom::Affine;
use filmcraft_media::FrameRequest;
use filmcraft_project::{ItemId, ItemKind, MediaRef, Project, Sequence, TrackItem};
use filmcraft_time::Tick;

use crate::gpufx::FxOp;
use crate::{Blend, RenderOptions, SourceProvider, motion_matrix, output_size};

/// Backend-neutral resources checked before strict GPU dispatch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceRequirements {
    pub sampled_inputs: u8,
    pub history: bool,
    pub mask: bool,
    pub lut: bool,
    pub textures: Vec<TextureRequirement>,
    pub compute: Option<ComputeRequirement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormatRequirement {
    Rgba16Float,
    Rgba32Float,
    R8Unorm,
    Rg8Unorm,
    R16Unorm,
    Rg16Unorm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureRequirement {
    pub format: TextureFormatRequirement,
    pub sampled: bool,
    pub storage: bool,
    pub render_attachment: bool,
    pub extent: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputeRequirement {
    pub workgroup_size: [u32; 3],
    pub storage_buffers: u32,
    pub storage_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Source,
    History,
    Mask,
    Lut,
    SampledInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameRequirement {
    MissingSequence,
    MissingItem,
    NestedCycle,
    NestedDepth,
    NestedComposite,
    NestedPlacement,
    ColorPipeline,
    InvalidDimensions,
    InvalidSourceData,
    InvalidScale,
    InvalidMotion,
    InterpolatedMedia,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceRequirement {
    PhysicalGpu,
    TextureDimension { required: u32 },
    SampledTextures { required: u32 },
    TextureFormat { format: TextureFormatRequirement },
    ComputeLimits,
    DeviceAvailable,
    StrictAdmission,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EligibilityFailure {
    UnknownId { id: String },
    UnsupportedOperation { id: String },
    UnsupportedFrameStructure { detail: FrameRequirement },
    InvalidParameter { id: String, parameter: String },
    MissingResource { id: String, resource: ResourceKind },
    UnsupportedAdapter { vendor: u32 },
    UnsupportedDeviceRequirement { requirement: DeviceRequirement },
    CpuFallbackRequired { id: Option<String> },
    RuntimeFailure { stage: &'static str, detail: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuPlanError {
    pub item_id: Option<ItemId>,
    pub id: Option<String>,
    pub reason: EligibilityFailure,
}

/// Stable only within one preflight receipt; transport as `(frame_index, id)` outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResidentSourceId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResidentSourceRequest {
    pub id: ResidentSourceId,
    pub item_id: ItemId,
    pub source_time: Tick,
    pub expected_size: (u32, u32),
    pub kind: ResidentSourceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentSourceKind {
    Decoded,
    Demo(filmcraft_media::DemoScene),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResidentFrameMetadata {
    pub id: ResidentSourceId,
    pub item_id: ItemId,
    pub width: u32,
    pub height: u32,
    pub color: filmcraft_color::ColorInfo,
}

impl std::fmt::Display for GpuPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "strict GPU plan rejected item {:?}, operation {:?}: {:?}", self.item_id, self.id, self.reason)
    }
}
impl std::error::Error for GpuPlanError {}

#[derive(Clone, Debug, PartialEq)]
pub enum PlannedGpuOp {
    Effect { id: String, op: FxOp, requirements: ResourceRequirements },
    Transition { id: String, op: crate::transitions::gpu_plan::TransitionOp, progress: f32, scale: f32, requirements: ResourceRequirements },
}

/// Provenance survives materialization; callers cannot mint or alter evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PixelOrigin {
    DecodedMediaSource,
    CpuGenerated { kind: &'static str },
    GpuGenerated { route: &'static str },
}

#[derive(Clone, Debug)]
pub struct PlanNodeEvidence {
    item_id: ItemId,
    registry_id: Option<String>,
    origin: PixelOrigin,
    operations: Vec<PlannedGpuOp>,
}
impl PlanNodeEvidence {
    pub fn item_id(&self) -> ItemId {
        self.item_id
    }
    pub fn registry_id(&self) -> Option<&str> {
        self.registry_id.as_deref()
    }
    pub fn origin(&self) -> &PixelOrigin {
        &self.origin
    }
    pub fn operations(&self) -> &[PlannedGpuOp] {
        &self.operations
    }
}

struct PreflightLayer {
    evidence: PlanNodeEvidence,
    project: Project,
    item: TrackItem,
    time: Tick,
    source_time: Tick,
    source_id: ResidentSourceId,
    source_kind: ResidentSourceKind,
    size: (u32, u32),
    matrix: Affine,
    opacity: f32,
    blend: Blend,
    color: filmcraft_color::ColorPipeline,
}

enum PreflightStep {
    Layers(Vec<PreflightLayer>),
    Transition { inputs: [Vec<PreflightLayer>; 2], evidence: PlanNodeEvidence },
}

/// Opaque whole-chain receipt. No source provider or pixel allocation is used to mint it.
pub struct PreflightedFrame {
    width: usize,
    height: usize,
    steps: Vec<PreflightStep>,
    source_requests: Vec<ResidentSourceRequest>,
}
impl PreflightedFrame {
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn source_requests(&self) -> &[ResidentSourceRequest] {
        &self.source_requests
    }
    pub fn nodes(&self) -> impl Iterator<Item = &PlanNodeEvidence> {
        self.steps.iter().flat_map(|step| match step {
            PreflightStep::Layers(layers) => layers.iter().map(|layer| &layer.evidence).collect::<Vec<_>>(),
            PreflightStep::Transition { inputs, evidence } => inputs.iter().flatten().map(|layer| &layer.evidence).chain(std::iter::once(evidence)).collect(),
        })
    }
}

/// A decoded leaf bound to its original opaque provenance.
pub struct RequiredLayer {
    layer: PlanLayer,
    evidence: PlanNodeEvidence,
}
impl RequiredLayer {
    pub fn layer(&self) -> &PlanLayer {
        &self.layer
    }
    pub fn evidence(&self) -> &PlanNodeEvidence {
        &self.evidence
    }
}

pub enum RequiredStep {
    Layers(Vec<RequiredLayer>),
    Transition { inputs: [Vec<RequiredLayer>; 2], evidence: PlanNodeEvidence },
}

/// Strict typed materialization. There is deliberately no whole-frame image variant.
pub struct RequiredFramePlan {
    width: usize,
    height: usize,
    steps: Vec<RequiredStep>,
}
impl RequiredFramePlan {
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    pub fn steps(&self) -> &[RequiredStep] {
        &self.steps
    }
    pub fn nodes(&self) -> impl Iterator<Item = &PlanNodeEvidence> {
        self.steps.iter().flat_map(|step| match step {
            RequiredStep::Layers(layers) => layers.iter().map(|layer| &layer.evidence).collect::<Vec<_>>(),
            RequiredStep::Transition { inputs, evidence } => inputs.iter().flatten().map(|layer| &layer.evidence).chain(std::iter::once(evidence)).collect(),
        })
    }
    pub fn layers(&self) -> impl Iterator<Item = &RequiredLayer> {
        self.steps.iter().flat_map(|step| match step {
            RequiredStep::Layers(layers) => layers.iter().collect::<Vec<_>>(),
            RequiredStep::Transition { inputs, .. } => inputs.iter().flatten().collect(),
        })
    }
}

/// Resident source layer containing metadata only; pixel and GPU resources remain caller-owned.
pub struct ResidentRequiredLayer {
    source_id: ResidentSourceId,
    source_kind: ResidentSourceKind,
    source_time: Tick,
    width: u32,
    height: u32,
    color: filmcraft_color::ColorInfo,
    matrix: Affine,
    opacity: f32,
    blend: Blend,
    fx: Option<Arc<LayerFx>>,
    input: Option<Arc<InputTransform>>,
    evidence: PlanNodeEvidence,
}
impl ResidentRequiredLayer {
    pub fn source_id(&self) -> ResidentSourceId {
        self.source_id
    }
    pub fn source_kind(&self) -> ResidentSourceKind {
        self.source_kind
    }
    pub fn demo_time_seconds(&self) -> Option<f32> {
        matches!(self.source_kind, ResidentSourceKind::Demo(_)).then(|| self.source_time.seconds() as f32)
    }
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn color(&self) -> filmcraft_color::ColorInfo {
        self.color
    }
    pub fn matrix(&self) -> &Affine {
        &self.matrix
    }
    pub fn opacity(&self) -> f32 {
        self.opacity
    }
    pub fn blend(&self) -> Blend {
        self.blend
    }
    pub fn fx(&self) -> Option<&Arc<LayerFx>> {
        self.fx.as_ref()
    }
    pub fn input(&self) -> Option<&Arc<InputTransform>> {
        self.input.as_ref()
    }
    pub fn evidence(&self) -> &PlanNodeEvidence {
        &self.evidence
    }
}

pub enum ResidentRequiredStep {
    Layers(Vec<ResidentRequiredLayer>),
    Transition { inputs: [Vec<ResidentRequiredLayer>; 2], evidence: PlanNodeEvidence },
}

/// Strict plan whose layers reference caller-owned NV12 textures only by receipt-local ID.
pub struct ResidentRequiredFramePlan {
    width: usize,
    height: usize,
    steps: Vec<ResidentRequiredStep>,
}
impl ResidentRequiredFramePlan {
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    pub fn steps(&self) -> &[ResidentRequiredStep] {
        &self.steps
    }
    pub fn nodes(&self) -> impl Iterator<Item = &PlanNodeEvidence> {
        self.steps.iter().flat_map(|step| match step {
            ResidentRequiredStep::Layers(layers) => layers.iter().map(|layer| &layer.evidence).collect::<Vec<_>>(),
            ResidentRequiredStep::Transition { inputs, evidence } => {
                inputs.iter().flatten().map(|layer| &layer.evidence).chain(std::iter::once(evidence)).collect()
            }
        })
    }
    pub fn layers(&self) -> impl Iterator<Item = &ResidentRequiredLayer> {
        self.steps.iter().flat_map(|step| match step {
            ResidentRequiredStep::Layers(layers) => layers.iter().collect::<Vec<_>>(),
            ResidentRequiredStep::Transition { inputs, .. } => inputs.iter().flatten().collect(),
        })
    }
}

fn rejected(item_id: Option<ItemId>, id: Option<String>, reason: EligibilityFailure) -> GpuPlanError {
    GpuPlanError { item_id, id, reason }
}
/// The clip span the effects may read (grow, shrink, spin and move animate over it).
fn clip_timing(seq: &Sequence, item: &TrackItem, t: Tick, source: (u32, u32)) -> crate::vfx::ClipTiming {
    crate::vfx::ClipTiming {
        clip_seconds: item.duration.seconds(),
        clip_offset: (t - item.start).seconds(),
        frame_rate: seq.settings.frame_rate.as_f64(),
        sequence_size: (seq.settings.width, seq.settings.height),
        source_size: source,
    }
}

fn structure(item_id: ItemId, detail: FrameRequirement) -> GpuPlanError {
    rejected(Some(item_id), None, EligibilityFailure::UnsupportedFrameStructure { detail })
}
fn cpu_required(item_id: ItemId, id: Option<String>) -> GpuPlanError {
    rejected(Some(item_id), id.clone(), EligibilityFailure::CpuFallbackRequired { id })
}

/// Validate the entire active frame using only project metadata, before any decode or rasterization.
pub fn preflight_frame_required(project: &Project, seq_id: ItemId, t: Tick, opts: RenderOptions) -> Result<PreflightedFrame, GpuPlanError> {
    if !opts.scale.is_finite() || opts.scale <= 0.0 {
        return Err(structure(seq_id, FrameRequirement::InvalidScale));
    }
    let seq = project.sequence(seq_id).ok_or_else(|| structure(seq_id, FrameRequirement::MissingSequence))?;
    let (width, height) = output_size(seq, opts.scale);
    let mut path = Vec::new();
    let mut steps = preflight_sequence(project, seq_id, seq, t, opts, &mut path)?;
    let source_requests = assign_resident_source_ids(&mut steps)?;
    Ok(PreflightedFrame { width, height, steps, source_requests })
}

fn assign_resident_source_ids(steps: &mut [PreflightStep]) -> Result<Vec<ResidentSourceRequest>, GpuPlanError> {
    fn assign(layers: &mut [PreflightLayer], requests: &mut Vec<ResidentSourceRequest>) -> Result<(), GpuPlanError> {
        for layer in layers {
            let id = ResidentSourceId(u32::try_from(requests.len()).map_err(|_| structure(layer.evidence.item_id, FrameRequirement::InvalidSourceData))?);
            layer.source_id = id;
            requests.push(ResidentSourceRequest {
                id,
                item_id: layer.evidence.item_id,
                source_time: layer.source_time,
                expected_size: layer.size,
                kind: layer.source_kind,
            });
        }
        Ok(())
    }
    let mut requests = Vec::new();
    for step in steps {
        match step {
            PreflightStep::Layers(layers) => assign(layers, &mut requests)?,
            PreflightStep::Transition { inputs, .. } => {
                for layers in inputs {
                    assign(layers, &mut requests)?;
                }
            }
        }
    }
    Ok(requests)
}

fn preflight_sequence(
    project: &Project,
    seq_id: ItemId,
    seq: &Sequence,
    t: Tick,
    opts: RenderOptions,
    path: &mut Vec<ItemId>,
) -> Result<Vec<PreflightStep>, GpuPlanError> {
    if path.contains(&seq_id) {
        return Err(structure(seq_id, FrameRequirement::NestedCycle));
    }
    if path.len() >= crate::MAX_NEST_DEPTH as usize {
        return Err(structure(seq_id, FrameRequirement::NestedDepth));
    }
    if !seq.settings.color.is_plain() {
        return Err(structure(seq_id, FrameRequirement::ColorPipeline));
    }
    if seq.settings.width == 0 || seq.settings.height == 0 {
        return Err(structure(seq_id, FrameRequirement::InvalidDimensions));
    }
    if opts.captions && seq.caption_tracks.iter().any(|track| track.enabled && track.captions.iter().any(|caption| caption.range().contains(t))) {
        return Err(cpu_required(seq_id, Some("captions".into())));
    }
    path.push(seq_id);
    let result = (|| {
        let mut steps = Vec::new();
        for track in seq.video_tracks.iter().filter(|track| track.enabled) {
            if let Some(transition) = track.transitions.iter().find(|transition| transition.range().contains(t)) {
                let planned = crate::transitions::gpu_plan::evaluate(&transition.effect, transition.progress(t) as f32, opts.scale)
                    .map_err(|error| transition_error(seq_id, error))?;
                let mut inputs = [Vec::new(), Vec::new()];
                for (slot, id) in inputs.iter_mut().zip([transition.from, transition.to]) {
                    if let Some(id) = id {
                        let item = track.item(id).ok_or_else(|| structure(seq_id, FrameRequirement::MissingItem))?;
                        if item.enabled {
                            *slot = preflight_item(project, seq, item, t, opts, Some(Blend::Normal), path)?;
                        }
                    }
                }
                let progress = if transition.reverse {
                    inputs.swap(0, 1);
                    1.0 - planned.progress
                } else {
                    planned.progress
                };
                let evidence = PlanNodeEvidence {
                    item_id: seq_id,
                    registry_id: Some(planned.id.clone()),
                    origin: PixelOrigin::GpuGenerated { route: "transition" },
                    operations: vec![PlannedGpuOp::Transition {
                        id: planned.id,
                        op: planned.op,
                        progress,
                        scale: planned.scale,
                        requirements: planned.requirements,
                    }],
                };
                steps.push(PreflightStep::Transition { inputs, evidence });
            } else if let Some(item) = track.item_at(t).filter(|item| item.enabled) {
                steps.push(PreflightStep::Layers(preflight_item(project, seq, item, t, opts, None, path)?));
            }
        }
        Ok(steps)
    })();
    path.pop();
    result
}

fn transition_error(item: ItemId, error: crate::transitions::gpu_plan::TransitionPlanError) -> GpuPlanError {
    use crate::transitions::gpu_plan::TransitionPlanError;
    let (id, reason) = match error {
        TransitionPlanError::UnknownId { id } => (id.clone(), EligibilityFailure::UnknownId { id }),
        TransitionPlanError::UnsupportedOperation { id } => (id.clone(), EligibilityFailure::UnsupportedOperation { id }),
        TransitionPlanError::InvalidParameter { id, parameter } => (id.clone(), EligibilityFailure::InvalidParameter { id, parameter: parameter.into() }),
    };
    rejected(Some(item), Some(id), reason)
}

fn preflight_item(
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    override_blend: Option<Blend>,
    path: &mut Vec<ItemId>,
) -> Result<Vec<PreflightLayer>, GpuPlanError> {
    let source = project.item(item.item).ok_or_else(|| structure(item.item, FrameRequirement::MissingItem))?;
    if item.has_opacity_masks() {
        return Err(cpu_required(item.item, Some("opacity".into())));
    }
    for effect in item.effects.iter().filter(|effect| effect.enabled && (opts.effects || effect.def().is_some_and(|def| def.intrinsic))) {
        let Some(def) = filmcraft_project::find_effect(&effect.effect).filter(|def| def.kind == filmcraft_project::EffectKind::Video) else {
            return Err(rejected(Some(item.item), Some(effect.effect.clone()), EligibilityFailure::UnknownId { id: effect.effect.clone() }));
        };
        if (opts.effects || def.intrinsic) && effect.masks.iter().any(|mask| mask.mode != filmcraft_project::MaskMode::None) {
            return Err(cpu_required(item.item, Some(effect.effect.clone())));
        }
    }
    let mt = item.effect_time_at(t);
    let (opacity, own_blend) = crate::opacity_blend(item, mt);
    let blend = override_blend.unwrap_or(own_blend);
    if !opacity.is_finite() {
        return Err(structure(item.item, FrameRequirement::InvalidMotion));
    }
    if let ItemKind::Sequence(nested) = &source.kind {
        let ft = item.source_time_at(t);
        // Nested isolated composites need a GPU subpass; never flatten them on the CPU.
        let nested_opts = RenderOptions { captions: true, ..opts };
        let nested_steps = preflight_sequence(project, item.item, nested, ft, nested_opts, path)?;
        if item.multicam_angle(nested).is_some()
            || (opts.effects && item.has_standard_effects())
            || blend != Blend::Normal
            || opacity < 1.0 - 1e-6
            || !near_identity(&motion_matrix(seq, item, (nested.settings.width, nested.settings.height), Some(nested.settings.par), mt))
            || !nest_is_plain(project, seq, nested, ft)
        {
            return Err(structure(item.item, FrameRequirement::NestedPlacement));
        }
        return nested_steps.into_iter().try_fold(Vec::new(), |mut layers, step| match step {
            PreflightStep::Layers(mut next) => {
                layers.append(&mut next);
                Ok(layers)
            }
            PreflightStep::Transition { .. } => Err(structure(item.item, FrameRequirement::NestedComposite)),
        });
    }
    if !matches!(source.kind, ItemKind::Media(_) | ItemKind::Subclip { .. }) {
        return Err(cpu_required(item.item, None));
    }
    let (_, media, _) = project.resolve_media(item.item).ok_or_else(|| structure(item.item, FrameRequirement::MissingItem))?;
    let source_kind = match &media.media {
        MediaRef::File { .. } => ResidentSourceKind::Decoded,
        MediaRef::Generator(filmcraft_media::Generator::Demo(scene)) => ResidentSourceKind::Demo(*scene),
        MediaRef::Generator(_) => return Err(cpu_required(item.item, Some("generator".into()))),
    };
    if media.offline {
        return Err(rejected(Some(item.item), None, EligibilityFailure::MissingResource { id: item.item.0.to_string(), resource: ResourceKind::Source }));
    }
    let source_rate = media.info.frame_rate();
    let raw_source_time = crate::video_source_time(item, t, source_rate);
    let source_time = if let ResidentSourceKind::Demo(_) = source_kind {
        let time = source_rate.tick_of(source_rate.frame_at(raw_source_time.max(Tick::ZERO)));
        let seconds = time.seconds() as f32;
        if !seconds.is_finite() || time >= media.info.duration || seconds >= 18.0 {
            return Err(structure(item.item, FrameRequirement::InvalidSourceData));
        }
        if media.interpret.color_space.is_some() {
            return Err(structure(item.item, FrameRequirement::ColorPipeline));
        }
        time
    } else {
        raw_source_time
    };
    if crate::interpolation_blend(item, t, media.info.frame_rate()).is_some() {
        return Err(structure(item.item, FrameRequirement::InterpolatedMedia));
    }
    let size =
        crate::source_size(project, item.item).filter(|(w, h)| *w > 0 && *h > 0).ok_or_else(|| structure(item.item, FrameRequirement::InvalidDimensions))?;
    let motion = motion_matrix(seq, item, size, crate::source_par(project, item.item), mt);
    if ![motion.a, motion.b, motion.c, motion.d, motion.e, motion.f].iter().all(|x| x.is_finite()) {
        return Err(structure(item.item, FrameRequirement::InvalidMotion));
    }
    let timing = clip_timing(seq, item, t, size);
    let cx = crate::effects::FxCtx {
        t: mt,
        px_scale: 1.0,
        seconds: (t - item.start).seconds(),
        timecode: "",
        clip_name: &item.name,
        project: Some(project),
        env: Some(&timing),
        frame_rate: seq.settings.frame_rate.as_f64(),
        working: seq.settings.color.working,
    };
    let mut operations = Vec::new();
    if opts.effects {
        for effect in item.effects.iter().filter(|effect| effect.enabled && effect.def().is_some_and(|def| !def.intrinsic)) {
            let requirements = crate::gpufx::effect_requirements(effect);
            let missing = if requirements.history {
                Some(ResourceKind::History)
            } else if requirements.mask {
                Some(ResourceKind::Mask)
            } else if requirements.lut {
                Some(ResourceKind::Lut)
            } else if requirements.sampled_inputs > 1 {
                Some(ResourceKind::SampledInput)
            } else {
                None
            };
            if let Some(resource) = missing {
                return Err(rejected(
                    Some(item.item),
                    Some(effect.effect.clone()),
                    EligibilityFailure::MissingResource { id: effect.effect.clone(), resource },
                ));
            }
            use crate::gpufx::{EffectGpuFailure, EffectGpuOutcome};
            match crate::gpufx::evaluate_gpu_effect(effect, &cx, size.0 as usize, size.1 as usize) {
                EffectGpuOutcome::Eligible(op) => operations.push(PlannedGpuOp::Effect { id: effect.effect.clone(), op, requirements }),
                EffectGpuOutcome::Disabled => {}
                EffectGpuOutcome::Ineligible(reason) => {
                    let id = effect.effect.clone();
                    let reason = match reason {
                        EffectGpuFailure::UnknownId => EligibilityFailure::UnknownId { id: id.clone() },
                        EffectGpuFailure::InvalidParameter { parameter } => EligibilityFailure::InvalidParameter { id: id.clone(), parameter },
                        EffectGpuFailure::MissingResource { resource } => EligibilityFailure::MissingResource { id: id.clone(), resource },
                        EffectGpuFailure::UnsupportedOperation | EffectGpuFailure::UnsupportedCase => {
                            EligibilityFailure::UnsupportedOperation { id: id.clone() }
                        }
                    };
                    return Err(rejected(Some(item.item), Some(id), reason));
                }
            }
        }
    }
    let origin = match source_kind {
        ResidentSourceKind::Decoded => PixelOrigin::DecodedMediaSource,
        ResidentSourceKind::Demo(_) => PixelOrigin::GpuGenerated { route: "demo_source" },
    };
    let evidence = PlanNodeEvidence { item_id: item.item, registry_id: None, origin, operations };
    Ok(vec![PreflightLayer {
        evidence,
        project: project.clone(),
        item: item.clone(),
        time: t,
        source_time,
        source_id: ResidentSourceId(0),
        source_kind,
        size,
        matrix: Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion),
        opacity,
        blend,
        color: seq.settings.color,
    }])
}

/// Consume the receipt before obtaining any source. Errors never omit a layer or enter CPU rendering.
pub fn materialize_frame_required(receipt: PreflightedFrame, sources: &dyn SourceProvider) -> Result<RequiredFramePlan, GpuPlanError> {
    if let Some(request) = receipt.source_requests.iter().find(|request| matches!(request.kind, ResidentSourceKind::Demo(_))) {
        return Err(cpu_required(request.item_id, Some("GPU-generated demo source requires resident materialization".into())));
    }
    let mut steps = Vec::new();
    for step in receipt.steps {
        steps.push(match step {
            PreflightStep::Layers(layers) => {
                RequiredStep::Layers(layers.into_iter().map(|layer| materialize_required_layer(layer, sources)).collect::<Result<_, _>>()?)
            }
            PreflightStep::Transition { inputs: [a, b], evidence } => RequiredStep::Transition {
                inputs: [
                    a.into_iter().map(|layer| materialize_required_layer(layer, sources)).collect::<Result<_, _>>()?,
                    b.into_iter().map(|layer| materialize_required_layer(layer, sources)).collect::<Result<_, _>>()?,
                ],
                evidence,
            },
        });
    }
    Ok(RequiredFramePlan { width: receipt.width, height: receipt.height, steps })
}

/// Strict plan for a frame whose sources are all GPU-generated demo scenes: nothing is decoded or
/// read from the CPU, so the metadata is fixed (generated frames are always full-range sRGB).
pub fn materialize_frame_required_demo(receipt: PreflightedFrame) -> Result<ResidentRequiredFramePlan, GpuPlanError> {
    let frames: Vec<ResidentFrameMetadata> = receipt
        .source_requests
        .iter()
        .map(|request| ResidentFrameMetadata {
            id: request.id,
            item_id: request.item_id,
            width: request.expected_size.0,
            height: request.expected_size.1,
            color: filmcraft_color::ColorInfo::SRGB_FULL,
        })
        .collect();
    materialize_frame_required_resident(receipt, &frames)
}

/// Materialize a strict resident plan from metadata only. This route never accepts decoded pixels.
pub fn materialize_frame_required_resident(receipt: PreflightedFrame, frames: &[ResidentFrameMetadata]) -> Result<ResidentRequiredFramePlan, GpuPlanError> {
    if frames.len() != receipt.source_requests.len() {
        return Err(rejected(
            None,
            Some("resident NV12 source bindings".into()),
            EligibilityFailure::InvalidParameter { id: "resident NV12 source bindings".into(), parameter: "exact receipt source coverage".into() },
        ));
    }
    let mut by_id = HashMap::with_capacity(frames.len());
    for frame in frames {
        if by_id.insert(frame.id, frame).is_some() {
            return Err(rejected(
                Some(frame.item_id),
                Some("resident NV12 source".into()),
                EligibilityFailure::InvalidParameter { id: "resident NV12 source".into(), parameter: "duplicate receipt source id".into() },
            ));
        }
    }
    for request in &receipt.source_requests {
        let frame = by_id.get(&request.id).ok_or_else(|| {
            rejected(Some(request.item_id), None, EligibilityFailure::MissingResource { id: request.id.0.to_string(), resource: ResourceKind::Source })
        })?;
        if frame.item_id != request.item_id || (frame.width, frame.height) != request.expected_size {
            return Err(structure(request.item_id, FrameRequirement::InvalidDimensions));
        }
        if frame.width == 0
            || frame.height == 0
            || (request.kind == ResidentSourceKind::Decoded && (!frame.width.is_multiple_of(2) || !frame.height.is_multiple_of(2)))
        {
            return Err(structure(request.item_id, FrameRequirement::InvalidSourceData));
        }
        if matches!(request.kind, ResidentSourceKind::Demo(_)) && frame.color != filmcraft_color::ColorInfo::SRGB_FULL {
            return Err(structure(request.item_id, FrameRequirement::ColorPipeline));
        }
    }

    fn layer(planned: PreflightLayer, metadata: &ResidentFrameMetadata) -> Result<ResidentRequiredLayer, GpuPlanError> {
        let item_id = planned.evidence.item_id;
        if planned.source_kind == ResidentSourceKind::Decoded && planned.evidence.origin != PixelOrigin::DecodedMediaSource {
            return Err(structure(item_id, FrameRequirement::InvalidSourceData));
        }
        if matches!(planned.source_kind, ResidentSourceKind::Demo(_)) && planned.evidence.origin != (PixelOrigin::GpuGenerated { route: "demo_source" }) {
            return Err(structure(item_id, FrameRequirement::InvalidSourceData));
        }
        let generated = matches!(planned.source_kind, ResidentSourceKind::Demo(_));
        let source_space = crate::colorman::source_space_from_info(&planned.project, item_id, metadata.color);
        let input = if !generated && crate::colorman::needs_management_from_info(&planned.color, source_space, metadata.color) {
            Some(
                crate::colorman::gpu_input_nv12(&planned.project, item_id, &planned.color, source_space, metadata.color)
                    .ok_or_else(|| structure(item_id, FrameRequirement::ColorPipeline))?,
            )
        } else {
            None
        };
        let ops = planned
            .evidence
            .operations
            .iter()
            .filter_map(|operation| match operation {
                PlannedGpuOp::Effect { op, .. } => Some(op.clone()),
                PlannedGpuOp::Transition { .. } => None,
            })
            .collect::<Vec<_>>();
        let fx = (!ops.is_empty()).then(|| Arc::new(LayerFx { size: planned.size, decimation: 1, ops }));
        Ok(ResidentRequiredLayer {
            source_id: planned.source_id,
            source_kind: planned.source_kind,
            source_time: planned.source_time,
            width: metadata.width,
            height: metadata.height,
            color: metadata.color,
            matrix: planned.matrix,
            opacity: planned.opacity,
            blend: planned.blend,
            fx,
            input,
            evidence: planned.evidence,
        })
    }

    fn take_layer(planned: PreflightLayer, metadata: &HashMap<ResidentSourceId, &ResidentFrameMetadata>) -> Result<ResidentRequiredLayer, GpuPlanError> {
        let id = planned.source_id;
        let frame = metadata.get(&id).ok_or_else(|| {
            rejected(Some(planned.evidence.item_id), None, EligibilityFailure::MissingResource { id: id.0.to_string(), resource: ResourceKind::Source })
        })?;
        layer(planned, frame)
    }

    let mut steps = Vec::with_capacity(receipt.steps.len());
    for step in receipt.steps {
        steps.push(match step {
            PreflightStep::Layers(layers) => ResidentRequiredStep::Layers(layers.into_iter().map(|layer| take_layer(layer, &by_id)).collect::<Result<_, _>>()?),
            PreflightStep::Transition { inputs: [a, b], evidence } => ResidentRequiredStep::Transition {
                inputs: [
                    a.into_iter().map(|layer| take_layer(layer, &by_id)).collect::<Result<_, _>>()?,
                    b.into_iter().map(|layer| take_layer(layer, &by_id)).collect::<Result<_, _>>()?,
                ],
                evidence,
            },
        });
    }
    Ok(ResidentRequiredFramePlan { width: receipt.width, height: receipt.height, steps })
}

fn materialize_required_layer(planned: PreflightLayer, sources: &dyn SourceProvider) -> Result<RequiredLayer, GpuPlanError> {
    let id = planned.evidence.item_id;
    let src = sources
        .source(id)
        .ok_or_else(|| rejected(Some(id), None, EligibilityFailure::MissingResource { id: id.0.to_string(), resource: ResourceKind::Source }))?;
    let time = crate::video_source_time(&planned.item, planned.time, src.info().frame_rate());
    let frame = src
        .video_frame(FrameRequest { time, scale: 1.0 })
        .map_err(|error| rejected(Some(id), None, EligibilityFailure::RuntimeFailure { stage: "decode", detail: error.to_string() }))?;
    if (frame.width, frame.height) != planned.size {
        return Err(structure(id, FrameRequirement::InvalidDimensions));
    }
    // Validate buffer metadata before the upload boundary; never inspect or rewrite pixels.
    let pixels = (frame.width as usize).checked_mul(frame.height as usize).ok_or_else(|| structure(id, FrameRequirement::InvalidSourceData))?;
    let valid = match &frame.data {
        filmcraft_frame::PixelData::Rgba8(data) => pixels.checked_mul(4) == Some(data.len()),
        filmcraft_frame::PixelData::RgbaF32(data) => pixels.checked_mul(4) == Some(data.len()),
        filmcraft_frame::PixelData::Yuv8 { planes, chroma, alpha } => {
            let (x, y) = chroma.shifts();
            let chroma_pixels = (frame.width.div_ceil(1 << x) as usize).checked_mul(frame.height.div_ceil(1 << y) as usize);
            planes[0].len() == pixels
                && chroma_pixels == Some(planes[1].len())
                && chroma_pixels == Some(planes[2].len())
                && alpha.as_ref().is_none_or(|data| data.len() == pixels)
        }
        filmcraft_frame::PixelData::Yuv16 { planes, chroma, bits, alpha } => {
            let (x, y) = chroma.shifts();
            let chroma_pixels = (frame.width.div_ceil(1 << x) as usize).checked_mul(frame.height.div_ceil(1 << y) as usize);
            (9..=16).contains(bits)
                && planes[0].len() == pixels
                && chroma_pixels == Some(planes[1].len())
                && chroma_pixels == Some(planes[2].len())
                && alpha.as_ref().is_none_or(|data| data.len() == pixels)
        }
    };
    if !valid {
        return Err(structure(id, FrameRequirement::InvalidSourceData));
    }
    let cs = crate::colorman::source_space(&planned.project, id, &frame);
    let input = if crate::colorman::needs_management(&planned.color, cs, &frame) {
        Some(crate::colorman::gpu_input(&planned.project, id, &planned.color, cs, &frame).ok_or_else(|| structure(id, FrameRequirement::ColorPipeline))?)
    } else {
        None
    };
    let ops = planned
        .evidence
        .operations
        .iter()
        .filter_map(|op| match op {
            PlannedGpuOp::Effect { op, .. } => Some(op.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let fx = (!ops.is_empty()).then(|| Arc::new(LayerFx { size: planned.size, decimation: 1, ops }));
    Ok(RequiredLayer {
        layer: PlanLayer { frame, matrix: planned.matrix, opacity: planned.opacity, blend: planned.blend, fx, input },
        evidence: planned.evidence,
    })
}

/// A GPU frame plan retains every resolved source frame. Nested sequences may share the same
/// child sequence many times, so recursion depth alone does not bound this expansion.
const MAX_PLAN_LAYERS: usize = 4096;

/// One layer for the GPU, bottom to top.
#[derive(Clone)]
pub struct PlanLayer {
    pub frame: Arc<VideoFrame>,
    /// Maps frame pixels (0..w, 0..h) to output pixels — or, with [`fx`](Self::fx), working-image
    /// pixels (0..fx.size).
    pub matrix: Affine,
    pub opacity: f32,
    /// How the layer combines with what is under it ([`crate::blend::composite`]).
    pub blend: Blend,
    /// Standard effects to run on the frame before it is placed (None: draw the frame directly).
    pub fx: Option<Arc<LayerFx>>,
    /// Colour management of a Y'CbCr frame that needs more than the default decode (HDR, BT.2020,
    /// P3 media on a Rec. 709 sequence; [`crate::colorman::gpu_input`]): run on the GPU as on the
    /// CPU. None: the default decode.
    pub input: Option<Arc<filmcraft_color::InputTransform>>,
}

/// The GPU effect stage of a layer: the frame is decoded into a working image of `size` (the
/// frame box-decimated by `decimation`, as the CPU decodes it), `ops` run on it in order, and the
/// result is placed with the layer's matrix, opacity and blend mode.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerFx {
    pub size: (u32, u32),
    pub decimation: u32,
    pub ops: Vec<FxOp>,
}

impl PlanLayer {
    /// A layer drawing `frame` directly.
    pub fn new(frame: Arc<VideoFrame>, matrix: Affine, opacity: f32, blend: Blend) -> Self {
        Self { frame, matrix, opacity, blend, fx: None, input: None }
    }

    /// Size of the picture the matrix places: the working image with effects, else the frame.
    pub fn size(&self) -> (u32, u32) {
        match &self.fx {
            Some(fx) => fx.size,
            None => (self.frame.width, self.frame.height),
        }
    }
}

#[derive(Clone)]
pub enum FramePlan {
    /// Draw these layers over black.
    Layers { width: usize, height: usize, layers: Vec<PlanLayer> },
    /// Ordered layer runs and two-input operations; each step composites onto the same canvas.
    Composite { width: usize, height: usize, steps: Vec<PlanStep> },
    /// The CPU produced the final image (fallback).
    Image(crate::Image),
}

#[derive(Clone)]
pub enum PlanStep {
    Layers(Vec<PlanLayer>),
    /// Inputs are isolated clip canvases; transition result composites Normal over lower tracks.
    Transition {
        inputs: [Vec<PlanLayer>; 2],
        effect: filmcraft_project::EffectInstance,
        progress: f32,
        scale: f32,
    },
}

#[derive(Default)]
struct PlanStack {
    steps: Vec<PlanStep>,
    exhausted: bool,
    /// Layers held in `steps`, kept as they are pushed so the cap check stays O(1).
    layers: usize,
}

impl PlanStack {
    fn layer_count(&self) -> usize {
        self.layers
    }

    fn push(&mut self, layer: PlanLayer) {
        if self.exhausted {
            return;
        }
        if self.layer_count() >= MAX_PLAN_LAYERS {
            self.exhausted = true;
            return;
        }
        if let Some(PlanStep::Layers(layers)) = self.steps.last_mut() {
            layers.push(layer);
        } else {
            self.steps.push(PlanStep::Layers(vec![layer]));
        }
        self.layers = self.layers.saturating_add(1);
    }

    fn push_transition(&mut self, inputs: [Vec<PlanLayer>; 2], effect: filmcraft_project::EffectInstance, progress: f32, scale: f32) {
        let added: usize = inputs.iter().map(Vec::len).sum();
        if self.exhausted || self.layer_count().saturating_add(added) > MAX_PLAN_LAYERS {
            self.exhausted = true;
            return;
        }
        self.layers = self.layers.saturating_add(added);
        self.steps.push(PlanStep::Transition { inputs, effect, progress, scale });
    }

    fn into_layers(self, width: usize, height: usize) -> Option<Vec<PlanLayer>> {
        if self.exhausted {
            return None;
        }
        match self.finish(width, height) {
            FramePlan::Layers { layers, .. } => Some(layers),
            plan => Some(vec![PlanLayer::new(cpu_frame(execute_cpu(&plan)), Affine::IDENTITY, 1.0, Blend::Normal)]),
        }
    }

    fn finish(mut self, width: usize, height: usize) -> FramePlan {
        if self.steps.len() <= 1 && self.steps.first().is_none_or(|s| matches!(s, PlanStep::Layers(_))) {
            let layers = match self.steps.pop() {
                Some(PlanStep::Layers(layers)) => layers,
                _ => Vec::new(),
            };
            FramePlan::Layers { width, height, layers }
        } else {
            FramePlan::Composite { width, height, steps: self.steps }
        }
    }
}

impl FramePlan {
    /// Source storage retained by the plan, before GPU preparation.
    pub fn source_bytes(&self) -> usize {
        let layers = |ls: &[PlanLayer]| ls.iter().map(|l| l.frame.byte_size()).sum::<usize>();
        match self {
            Self::Image(img) => img.px.len() * size_of::<f32>(),
            Self::Layers { layers: ls, .. } => layers(ls),
            Self::Composite { steps, .. } => steps
                .iter()
                .map(|s| match s {
                    PlanStep::Layers(ls) => layers(ls),
                    PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| layers(ls)).sum(),
                })
                .sum(),
        }
    }

    /// Largest output/source texture side required by the plan.
    pub fn max_side(&self) -> usize {
        let layer_side = |ls: &[PlanLayer]| ls.iter().map(|l| l.frame.width.max(l.frame.height) as usize).max().unwrap_or(0);
        match self {
            Self::Image(img) => img.w.max(img.h),
            Self::Layers { width, height, layers } => (*width).max(*height).max(layer_side(layers)),
            Self::Composite { width, height, steps } => steps
                .iter()
                .map(|s| match s {
                    PlanStep::Layers(ls) => layer_side(ls),
                    PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| layer_side(ls)).max().unwrap_or(0),
                })
                .fold((*width).max(*height), usize::max),
        }
    }
}

fn cpu_frame(img: crate::Image) -> Arc<VideoFrame> {
    Arc::new(VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px))
}

fn simple_transition(id: &str) -> bool {
    matches!(id, "cross_dissolve" | "dip_to_black" | "dip_to_white" | "blur_dissolve" | "flare")
}

/// The standard effects of a media clip if the GPU can draw it (any blend mode, no opacity masks,
/// every enabled standard effect unmasked and with a GPU implementation; empty without effects or
/// with effects off). None: the clip is rendered on the CPU.
fn gpu_chain<'a>(project: &Project, item: &'a TrackItem, opts: RenderOptions) -> Option<Vec<&'a filmcraft_project::EffectInstance>> {
    let is_media = project.item(item.item).is_some_and(|p| matches!(p.kind, ItemKind::Media(_) | ItemKind::Subclip { .. }));
    if !is_media || item.has_opacity_masks() {
        return None;
    }
    if !opts.effects {
        return Some(Vec::new());
    }
    let mut chain = Vec::new();
    for e in item.effects.iter().filter(|e| e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e)) {
        if !e.enabled {
            continue;
        }
        let masked = e.masks.iter().any(|m| m.mode != filmcraft_project::MaskMode::None);
        if masked || !crate::gpufx::GPU_EFFECTS.contains(&e.effect.as_str()) {
            return None;
        }
        chain.push(e);
    }
    Some(chain)
}

/// Plan the frame at timeline `t`.
pub fn plan_frame(project: &Project, seq_id: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> FramePlan {
    let Some(seq) = project.sequence(seq_id) else { return FramePlan::Image(crate::Image::new(1, 1)) };
    let (w, h) = output_size(seq, opts.scale);
    if whole_frame_on_cpu(project, seq, t) {
        return FramePlan::Image(crate::render_sequence(project, seq_id, t, opts, sources));
    }
    let mut layers = PlanStack::default();
    push_tracks(project, seq, t, opts, sources, 0, &mut layers);
    if layers.exhausted {
        return FramePlan::Image(crate::render_sequence(project, seq_id, t, opts, sources));
    }
    if opts.captions {
        for o in crate::caption_overlays(seq, t, w, h) {
            layers.push(PlanLayer::new(
                Arc::new(VideoFrame::rgba_f32(o.w as u32, o.h as u32, o.px)),
                Affine::translate(o.x as f64, o.y as f64),
                1.0,
                Blend::Normal,
            ));
        }
    }
    if layers.exhausted {
        return FramePlan::Image(crate::render_sequence(project, seq_id, t, opts, sources));
    }
    layers.finish(w, h)
}

/// Whether [`plan_frame`] hands the frame at `t` back as one CPU image ([`FramePlan::Image`]). A
/// GPU renderer gains nothing on such a frame, so a caller with only a few of them (the export's
/// pool) can render it on the CPU without taking one.
pub fn is_cpu_frame(project: &Project, seq_id: ItemId, t: Tick) -> bool {
    project.sequence(seq_id).is_none_or(|seq| whole_frame_on_cpu(project, seq, t))
}

/// The frames [`plan_frame`] does not plan: an HDR / wide-gamut sequence composites and converts
/// on the CPU, and so does a frame with an adjustment layer or a complex transition anywhere at
/// `t` (whole-frame fallback).
fn whole_frame_on_cpu(project: &Project, seq: &Sequence, t: Tick) -> bool {
    !seq.settings.color.is_plain() || !layered_at(project, seq, t)
}

/// Whether the frame of `seq` at `t` can be planned as layers: no adjustment layer and no
/// transition the compositor cannot mix itself is showing (a transition into or out of an
/// adjustment layer counts, also on the side of the cut where the other clip is).
pub(crate) fn layered_at(project: &Project, seq: &Sequence, t: Tick) -> bool {
    let adjustment = |it: &TrackItem| project.item(it.item).is_some_and(|p| matches!(p.kind, ItemKind::AdjustmentLayer { .. }));
    for tr in &seq.video_tracks {
        if !tr.enabled {
            continue;
        }
        if let Some(trn) = tr.transitions.iter().find(|x| x.range().contains(t)) {
            if !simple_transition(&trn.effect.effect) && trn.effect.effect != "push" {
                return false;
            }
            if [trn.from, trn.to].into_iter().flatten().filter_map(|c| tr.item(c)).any(adjustment) {
                return false;
            }
        }
        if let Some(it) = tr.item_at(t)
            && adjustment(it)
        {
            return false;
        }
    }
    true
}

/// Push the layers of every video track of `seq` at `t`, bottom track first (`seq` must be
/// [`layered_at`] `t`). `nest` counts the nested sequences already followed to reach `seq`.
fn push_tracks(project: &Project, seq: &Sequence, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider, nest: u32, layers: &mut PlanStack) {
    let (w, h) = output_size(seq, opts.scale);
    for tr in &seq.video_tracks {
        if layers.exhausted {
            return;
        }
        if !tr.enabled {
            continue;
        }
        if let Some(trn) = tr.transitions.iter().find(|x| x.range().contains(t)) {
            // These dissolves are symmetric: Reverse (play B→A backwards) renders the same frames.
            let p = trn.progress(t) as f32;
            let a = trn.from.and_then(|id| tr.item(id)).filter(|i| i.enabled);
            let b = trn.to.and_then(|id| tr.item(id)).filter(|i| i.enabled);
            match trn.effect.effect.as_str() {
                "push" | "blur_dissolve" | "flare" => {
                    let mut inputs = [Vec::new(), Vec::new()];
                    for (slot, item) in inputs.iter_mut().zip([a, b]) {
                        if let Some(item) = item {
                            let mut input = PlanStack::default();
                            push_item(project, seq, item, t, opts, sources, 1.0, Some(Blend::Normal), nest, &mut input);
                            let Some(input_layers) = input.into_layers(w, h) else {
                                layers.exhausted = true;
                                return;
                            };
                            *slot = input_layers;
                        }
                    }
                    let progress = if trn.reverse {
                        inputs.swap(0, 1);
                        1.0 - p
                    } else {
                        p
                    };
                    layers.push_transition(inputs, trn.effect.clone(), progress, opts.scale);
                }
                "dip_to_black" | "dip_to_white" => {
                    let col = if trn.effect.effect == "dip_to_black" { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] };
                    layers.push(PlanLayer::new(Arc::new(VideoFrame::rgba_f32(1, 1, col.to_vec())), Affine::scale(w as f64, h as f64), 1.0, Blend::Normal));
                    let (it, k) = if p < 0.5 { (a, 1.0 - p * 2.0) } else { (b, (p - 0.5) * 2.0) };
                    if let Some(it) = it {
                        push_item(project, seq, it, t, opts, sources, k, Some(Blend::Normal), nest, layers);
                    }
                }
                _ => {
                    // cross dissolve: A at full, B over it at p (premultiplied over == linear mix when A is opaque)
                    if let Some(it) = a {
                        push_item(project, seq, it, t, opts, sources, 1.0 - if b.is_none() { p } else { 0.0 }, Some(Blend::Normal), nest, layers);
                    }
                    if let Some(it) = b {
                        push_item(project, seq, it, t, opts, sources, p, Some(Blend::Normal), nest, layers);
                    }
                }
            }
            continue;
        }
        let Some(item) = tr.item_at(t) else { continue };
        if !item.enabled {
            continue;
        }
        push_item(project, seq, item, t, opts, sources, 1.0, None, nest, layers);
    }
}

/// Whether a nested sequence can be drawn by pushing its own layers into the plan of the sequence
/// it is in, instead of being rendered to an image on the CPU first. Compositing its layers one
/// by one over what is below gives the same picture as compositing them together first only when
/// they all blend Normal, and the frame must be one that plans as layers in the same colour
/// pipeline.
fn nest_is_plain(project: &Project, seq: &Sequence, nested: &Sequence, ft: Tick) -> bool {
    nested.settings.color == seq.settings.color
        && (nested.settings.width, nested.settings.height) == (seq.settings.width, seq.settings.height)
        && layered_at(project, nested, ft)
        && !nested.video_tracks.iter().any(|tr| tr.enabled && tr.transitions.iter().any(|t| t.range().contains(ft) && !simple_transition(&t.effect.effect)))
        && nested.video_tracks.iter().filter(|tr| tr.enabled).all(|tr| {
            tr.transitions.iter().any(|x| x.range().contains(ft))
                || tr.item_at(ft).filter(|i| i.enabled).is_none_or(|i| crate::opacity_blend(i, i.effect_time_at(ft)).1 == Blend::Normal)
        })
}

/// Push the layer(s) of `item`. `blend` overrides the item's own blend mode: inside a transition
/// the CPU reference mixes the clips and composites the result Normal, ignoring their modes.
/// `nest` counts the multi-camera clips already followed to reach `item` (see
/// [`crate::MAX_NEST_DEPTH`]).
#[allow(clippy::too_many_arguments)]
fn push_item(
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
    extra_opacity: f32,
    blend: Option<Blend>,
    nest: u32,
    out: &mut PlanStack,
) {
    if out.exhausted {
        return;
    }
    // frame time (`ft`) vs. effect time (`mt`): they differ inside a frame hold without Hold Filters
    let ft = item.source_time_at(t);
    let mt = item.effect_time_at(t);
    let (op, own) = crate::opacity_blend(item, mt);
    let bl = blend.unwrap_or(own);
    // A multi-camera clip that only shows its angle (no effects, untransformed, same frame size)
    // draws the angle's clip directly: no CPU pass over the nested sequence.
    if let Some(ItemKind::Sequence(nested)) = project.item(item.item).map(|p| &p.kind)
        && nest < crate::MAX_NEST_DEPTH
        && let Some(angle) = item.multicam_angle(nested)
        && !(opts.effects && item.has_standard_effects())
        && (nested.settings.width, nested.settings.height) == (seq.settings.width, seq.settings.height)
        && near_identity(&motion_matrix(seq, item, (nested.settings.width, nested.settings.height), Some(nested.settings.par), mt))
        && let Some(tr) = nested.angle_video_track_index(angle).and_then(|i| nested.video_tracks.get(i))
        && !tr.transitions.iter().any(|x| x.range().contains(ft))
        && tr.item_at(ft).is_none_or(|i| crate::opacity_blend(i, i.effect_time_at(ft)).1 != Blend::Dissolve)
    {
        // Inside the nested sequence the angle's clip is composited onto an empty canvas, where
        // every mode but Dissolve is Normal; the multicam clip's own mode then applies to it.
        if let Some(inner) = tr.item_at(ft).filter(|i| i.enabled) {
            push_item(project, nested, inner, ft, opts, sources, extra_opacity * op, Some(bl), nest + 1, out);
        }
        return;
    }
    // A nested sequence that is only shown (no effects, untransformed, fully opaque, blending
    // Normal) contributes its own layers: the compositor draws them like the clips of this
    // sequence, with no CPU pass over the nested sequence. Its captions are part of its picture.
    if let Some(ItemKind::Sequence(nested)) = project.item(item.item).map(|p| &p.kind)
        && nest < crate::MAX_NEST_DEPTH
        && item.multicam_angle(nested).is_none()
        && !(opts.effects && item.has_standard_effects())
        && !item.has_opacity_masks()
        && bl == Blend::Normal
        && extra_opacity * op >= 1.0 - 1e-6
        && near_identity(&motion_matrix(seq, item, (nested.settings.width, nested.settings.height), Some(nested.settings.par), mt))
        && nest_is_plain(project, seq, nested, ft)
    {
        push_tracks(project, nested, ft, opts, sources, nest + 1, out);
        if out.exhausted {
            return;
        }
        let (w, h) = output_size(nested, opts.scale);
        for o in crate::caption_overlays(nested, ft, w, h) {
            out.push(PlanLayer::new(
                Arc::new(VideoFrame::rgba_f32(o.w as u32, o.h as u32, o.px)),
                Affine::translate(o.x as f64, o.y as f64),
                1.0,
                Blend::Normal,
            ));
        }
        return;
    }
    // Graphic clips without standard effects: the layers are rasterised (cached) into one tight
    // image the GPU places as a layer.
    if !(opts.effects && item.has_standard_effects() || item.has_opacity_masks())
        && project.item(item.item).is_some_and(|p| matches!(p.kind, ItemKind::Graphic { .. }))
    {
        let Some(size) = crate::source_size(project, item.item) else { return };
        let (w, h) = output_size(seq, opts.scale);
        let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion_matrix(seq, item, size, None, mt));
        if let Some((img, x, y)) = crate::graphic_clip::render_graphic_tight(item, mt, size, &m, w, h) {
            out.push(PlanLayer::new(cpu_frame(img), Affine::translate(x as f64, y as f64), op * extra_opacity, bl));
        }
        return;
    }
    if let Some(chain) = gpu_chain(project, item, opts) {
        let Some(src) = sources.source(item.item) else { return };
        let Some(size) = crate::source_size(project, item.item) else { return };
        let motion = motion_matrix(seq, item, size, crate::source_par(project, item.item), mt);
        let lin = ((motion.a * motion.a + motion.b * motion.b).sqrt()).max((motion.c * motion.c + motion.d * motion.d).sqrt());
        let want = (lin * opts.scale as f64).clamp(1.0 / 64.0, 1.0) as f32;
        let time = crate::video_source_time(item, t, src.info().frame_rate());
        let Ok(frame) = src.video_frame(FrameRequest { time, scale: want }) else { return };
        let cs = crate::colorman::source_space(project, item.item, &frame);
        // HDR and wide-gamut media on a Rec. 709 sequence is converted on the GPU too (#965); log
        // media and other pipelines are converted on the CPU (below), and so are blended
        // in-between frames (Frame Blending / Optical Flow on speed-changed clips)
        let pipe = &seq.settings.color;
        let input = if crate::colorman::needs_management(pipe, cs, &frame) { crate::colorman::gpu_input(project, item.item, pipe, cs, &frame) } else { None };
        let plain =
            (input.is_some() || !crate::colorman::needs_management(pipe, cs, &frame)) && crate::interpolation_blend(item, t, src.info().frame_rate()).is_none();
        if plain && !chain.is_empty() {
            // GPU effect stage: the working image the CPU would decode (`base_layer`), the
            // effects evaluated for it, placed as `item_layer` places it
            let n = crate::decimation(frame.width as f32, size.0 as f32 * want);
            let (lw, lh) = ((frame.width as usize / n).max(1), (frame.height as usize / n).max(1));
            let px_scale = lw as f32 / size.0.max(1) as f32;
            let tc = "";
            let timing = clip_timing(seq, item, t, size);
            let cx = crate::effects::FxCtx {
                t: mt,
                px_scale,
                seconds: (t - item.start).seconds(),
                timecode: tc,
                clip_name: &item.name,
                project: Some(project),
                env: Some(&timing),
                frame_rate: seq.settings.frame_rate.as_f64(),
                working: seq.settings.color.working,
            };
            let ops: Option<Vec<FxOp>> = chain.iter().map(|e| FxOp::eval(e, &cx, lw, lh).filter(FxOp::gpu_ok)).collect();
            if let Some(ops) = ops {
                let m = Affine::scale(opts.scale as f64, opts.scale as f64)
                    .then_apply(&motion)
                    .then_apply(&Affine::scale(1.0 / px_scale as f64, 1.0 / px_scale as f64));
                let fx = LayerFx { size: (lw as u32, lh as u32), decimation: n as u32, ops };
                out.push(PlanLayer { frame, matrix: m, opacity: op * extra_opacity, blend: bl, fx: Some(Arc::new(fx)), input });
                return;
            }
        } else if plain {
            // Draft playback at reduced resolution: hand over planes box-filtered to the size
            // drawn instead of the full picture (a quarter at 1/2, a sixteenth at 1/4 of the
            // upload and sampling). The mean is taken over Y'CbCr codes, not linear light as
            // the shader's supersampling does, so this stays limited to the opt-in draft mode.
            let n = if filmcraft_media::cancel::draft() { crate::decimation(frame.width as f32, size.0 as f32 * want) } else { 1 };
            let frame = match frame.box_decimated(n) {
                Some(small) => Arc::new(small),
                None => frame,
            };
            let px_scale = frame.width as f64 / size.0.max(1) as f64;
            let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion).then_apply(&Affine::scale(1.0 / px_scale, 1.0 / px_scale));
            out.push(PlanLayer { input, ..PlanLayer::new(frame, m, op * extra_opacity, bl) });
            return;
        }
    }
    // CPU-rendered layer (standard effects): drawn by the GPU as a pre-rendered canvas image.
    let tc = filmcraft_time::format_time(t, seq.settings.frame_rate, seq.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48_000);
    if let Some((img, op2, _)) = crate::item_layer(project, seq, item, t, opts, sources, &tc) {
        out.push(PlanLayer::new(cpu_frame(img), Affine::IDENTITY, op2 * extra_opacity, bl));
    }
}

fn near_identity(m: &Affine) -> bool {
    (m.a - 1.0).abs() < 1e-9 && (m.d - 1.0).abs() < 1e-9 && m.b.abs() < 1e-9 && m.c.abs() < 1e-9 && m.e.abs() < 1e-6 && m.f.abs() < 1e-6
}

/// A layer's working image with its effects applied, on the CPU (the reference for the GPU
/// effect stage).
pub fn effect_image(frame: &VideoFrame, fx: &LayerFx) -> crate::Image {
    effect_image_with(frame, fx, None)
}

/// [`effect_image`] of a colour-managed layer ([`PlanLayer::input`]).
pub fn effect_image_with(frame: &VideoFrame, fx: &LayerFx, input: Option<&filmcraft_color::InputTransform>) -> crate::Image {
    let mut img = linear_image(frame, fx.decimation.max(1) as usize, input);
    for op in &fx.ops {
        op.apply(&mut img);
    }
    img
}

/// Execute a plan on the CPU (reference for the GPU compositor).
pub fn execute_cpu(plan: &FramePlan) -> crate::Image {
    match plan {
        FramePlan::Image(img) => img.clone(),
        FramePlan::Composite { width, height, steps } => {
            let mut canvas = crate::Image::new(*width, *height);
            for step in steps {
                match step {
                    PlanStep::Layers(layers) => execute_layers(&mut canvas, layers),
                    PlanStep::Transition { inputs, effect, progress, scale } => {
                        let [a, b] = inputs.each_ref().map(|layers| {
                            let mut input = crate::Image::new(*width, *height);
                            execute_layers(&mut input, layers);
                            input
                        });
                        let mixed = crate::transitions::apply_scaled(effect, &a, &b, *progress, *scale);
                        crate::blend::composite(&mut canvas, &mixed, 1.0, Blend::Normal);
                    }
                }
            }
            canvas
        }
        FramePlan::Layers { width, height, layers } => {
            let mut canvas = crate::Image::new(*width, *height);
            execute_layers(&mut canvas, layers);
            canvas
        }
    }
}

/// A layer's frame (box-decimated by `n`) as a linear working image: the default decode, or
/// `input`'s table and pixel stage as `colorman::decode` runs them.
fn linear_image(frame: &VideoFrame, n: usize, input: Option<&filmcraft_color::InputTransform>) -> crate::Image {
    let (w, h, px) = frame.to_linear_f32_decimated_with(n, input.map(|t| &t.table));
    let mut img = crate::Image { w, h, px };
    if let Some(t) = input.filter(|t| t.has_pixel_stage()) {
        img.map_rgb(|c, _, _| t.apply(c));
    }
    img
}

fn execute_layers(canvas: &mut crate::Image, layers: &[PlanLayer]) {
    let (width, height) = (canvas.w, canvas.h);
    for l in layers {
        let src = match &l.fx {
            Some(fx) => effect_image_with(&l.frame, fx, l.input.as_deref()),
            None => linear_image(&l.frame, 1, l.input.as_deref()),
        };
        let placed = if l.matrix == Affine::scale(width as f64, height as f64) && src.w == 1 && src.h == 1 {
            crate::Image::filled(width, height, src.get(0, 0))
        } else {
            src.transformed(width, height, &l.matrix)
        };
        crate::blend::composite(canvas, &placed, l.opacity, l.blend);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resident_plan() -> (PreflightedFrame, ResidentFrameMetadata) {
        use filmcraft_media::{MediaInfo, MediaKind, VideoStreamInfo};
        use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

        let mut project = Project::new("resident");
        let seq = project.new_sequence("sequence", SequenceSettings { width: 8, height: 8, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 0, None);
        let item_id = project.add_item(
            "source.mov",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::File { path: "source.mov".into() },
                info: MediaInfo {
                    name: "source.mov".into(),
                    kind: MediaKind::Movie,
                    duration: Tick(TICKS_PER_SECOND),
                    video: Some(VideoStreamInfo {
                        width: 8,
                        height: 8,
                        frame_rate: FrameRate::FPS_24,
                        par: (1, 1),
                        codec: "h264".into(),
                        pixel_format: "nv12".into(),
                        color: filmcraft_color::ColorInfo::REC709,
                        has_alpha: false,
                        bitrate: None,
                        hdr: None,
                    }),
                    audio_streams: Vec::new(),
                    container: "MOV".into(),
                    start_timecode: None,
                    file_size: None,
                },
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: Vec::new(),
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        let clip = project
            .make_track_item(item_id, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND)), FrameRate::FPS_24)
            .expect("test clip");
        project.sequence_mut(seq).expect("test sequence").video_tracks[0].items.push(clip);
        let receipt = preflight_frame_required(&project, seq, Tick::ZERO, RenderOptions::default()).expect("metadata preflight");
        let request = *receipt.source_requests().first().expect("one visible source");
        let metadata = ResidentFrameMetadata { id: request.id, item_id: request.item_id, width: 8, height: 8, color: filmcraft_color::ColorInfo::REC709 };
        (receipt, metadata)
    }

    #[test]
    fn resident_plan_uses_exact_receipt_id_and_metadata_without_cpu_source() {
        let (receipt, metadata) = resident_plan();
        let request = *receipt.source_requests().first().expect("one source request");
        assert_eq!(request.id, ResidentSourceId(0));
        assert_eq!(request.item_id, metadata.item_id);
        assert_eq!(request.expected_size, (8, 8));
        let plan = materialize_frame_required_resident(receipt, &[metadata]).expect("resident plan");
        assert_eq!(plan.size(), (8, 8));
        let layers: Vec<_> = plan.layers().collect();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].source_id(), request.id);
        assert_eq!(layers[0].size(), (8, 8));
        assert!(layers[0].input().is_none());
    }

    #[test]
    fn resident_plan_rejects_missing_duplicate_wrong_and_odd_metadata() {
        let (receipt, _metadata) = resident_plan();
        assert!(materialize_frame_required_resident(receipt, &[]).is_err());

        let (receipt, metadata) = resident_plan();
        assert!(materialize_frame_required_resident(receipt, &[metadata, metadata]).is_err());

        let (receipt, mut metadata) = resident_plan();
        metadata.id = ResidentSourceId(u32::MAX);
        assert!(materialize_frame_required_resident(receipt, &[metadata]).is_err());

        let (receipt, mut metadata) = resident_plan();
        metadata.item_id = ItemId(u64::MAX);
        assert!(materialize_frame_required_resident(receipt, &[metadata]).is_err());

        let (receipt, mut metadata) = resident_plan();
        metadata.width = 7;
        assert!(materialize_frame_required_resident(receipt, &[metadata]).is_err());

        let (receipt, mut metadata) = resident_plan();
        metadata.width = 6;
        metadata.height = 6;
        assert!(materialize_frame_required_resident(receipt, &[metadata]).is_err());
    }

    #[test]
    fn strict_resident_plan_accepts_only_demo_generators_as_generated_sources() {
        use filmcraft_media::{DemoScene, Generator, MediaSource};
        use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

        let scene = DemoScene::Aurora;
        let source = filmcraft_media::generators::GeneratorSource::demo(scene);
        let mut project = Project::new("generated resident");
        let seq = project.new_sequence("sequence", SequenceSettings { width: 8, height: 7, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 0, None);
        let item_id = project.add_item(
            "Aurora",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(Generator::Demo(scene)),
                info: source.info().clone(),
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: Vec::new(),
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        let clip = project
            .make_track_item(item_id, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(20 * TICKS_PER_SECOND)), FrameRate::FPS_24)
            .expect("test clip");
        project.sequence_mut(seq).expect("test sequence").video_tracks[0].items.push(clip);
        let requested_time = Tick(TICKS_PER_SECOND + 7);
        let receipt = preflight_frame_required(&project, seq, requested_time, RenderOptions::default()).expect("demo preflight");
        let request = *receipt.source_requests().first().expect("one visible source");
        assert_eq!(request.kind, ResidentSourceKind::Demo(scene));
        let rate = source.info().frame_rate();
        let quantized_time = rate.tick_of(rate.frame_at(requested_time));
        assert_eq!(request.source_time, quantized_time);
        assert_eq!(request.expected_size, (1920, 1080));
        assert_eq!(receipt.nodes().next().map(PlanNodeEvidence::origin), Some(&PixelOrigin::GpuGenerated { route: "demo_source" }));

        let metadata = ResidentFrameMetadata {
            id: request.id,
            item_id: request.item_id,
            width: request.expected_size.0,
            height: request.expected_size.1,
            color: filmcraft_color::ColorInfo::SRGB_FULL,
        };
        let plan = materialize_frame_required_resident(receipt, &[metadata]).expect("metadata-only demo materialization");
        let layers: Vec<_> = plan.layers().collect();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].size(), request.expected_size);
        assert_eq!(layers[0].demo_time_seconds(), Some(quantized_time.seconds() as f32));
        assert_eq!(layers[0].evidence().origin(), &PixelOrigin::GpuGenerated { route: "demo_source" });

        let receipt = preflight_frame_required(&project, seq, requested_time, RenderOptions::default()).expect("second demo preflight");
        let no_cpu_source = |_item| -> Option<filmcraft_media::SharedSource> { panic!("resident demo must not ask CPU provider") };
        assert!(materialize_frame_required(receipt, &no_cpu_source).is_err());
        if let ItemKind::Media(media) = &mut project.item_mut(item_id).expect("demo project item").kind {
            media.info.duration = Tick(TICKS_PER_SECOND / 2);
        }
        assert!(preflight_frame_required(&project, seq, requested_time, RenderOptions::default()).is_err());
    }

    #[test]
    fn strict_resident_demo_metadata_rejects_wrong_color_and_other_generators_remain_closed() {
        use filmcraft_media::{DemoScene, Generator, MediaSource};
        use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

        let source = filmcraft_media::generators::GeneratorSource::demo(DemoScene::Forest);
        let mut project = Project::new("generated resident");
        let seq = project.new_sequence("sequence", SequenceSettings { width: 8, height: 8, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 0, None);
        let item_id = project.add_item(
            "Forest",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(Generator::Demo(DemoScene::Forest)),
                info: source.info().clone(),
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: Vec::new(),
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        let clip = project
            .make_track_item(item_id, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND)), FrameRate::FPS_24)
            .expect("test clip");
        project.sequence_mut(seq).expect("test sequence").video_tracks[0].items.push(clip);
        let receipt = preflight_frame_required(&project, seq, Tick::ZERO, RenderOptions::default()).expect("demo preflight");
        let request = *receipt.source_requests().first().expect("one visible source");
        let metadata = ResidentFrameMetadata {
            id: request.id,
            item_id: request.item_id,
            width: request.expected_size.0,
            height: request.expected_size.1,
            color: filmcraft_color::ColorInfo::REC709,
        };
        assert!(materialize_frame_required_resident(receipt, &[metadata]).is_err());
        if let ItemKind::Media(media) = &mut project.item_mut(item_id).expect("demo project item").kind {
            media.interpret.color_space = Some(filmcraft_color::ColorSpace::Rec709);
        }
        assert!(preflight_frame_required(&project, seq, Tick::ZERO, RenderOptions::default()).is_err());

        let mut bars = Project::new("bars");
        let bars_seq = bars.new_sequence("sequence", SequenceSettings { width: 8, height: 8, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 0, None);
        let bars_source = filmcraft_media::generators::GeneratorSource::new(Generator::BarsAndTone, 8, 8, FrameRate::FPS_24, Tick(TICKS_PER_SECOND));
        let bars_id = bars.add_item(
            "Bars",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(Generator::BarsAndTone),
                info: bars_source.info().clone(),
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: Vec::new(),
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        let bars_clip = bars
            .make_track_item(bars_id, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND)), FrameRate::FPS_24)
            .expect("test clip");
        bars.sequence_mut(bars_seq).expect("test sequence").video_tracks[0].items.push(bars_clip);
        assert!(preflight_frame_required(&bars, bars_seq, Tick::ZERO, RenderOptions::default()).is_err());
    }

    #[test]
    fn required_preflight_accepts_empty_frame_without_source_provider() {
        let mut project = Project::new("strict");
        let seq = project.new_sequence("sequence", Default::default(), 1, 0, None);
        let receipt = preflight_frame_required(&project, seq, Tick::ZERO, RenderOptions::default()).unwrap();
        assert_eq!(receipt.nodes().count(), 0);
    }

    #[test]
    fn required_preflight_reports_missing_sequence() {
        let project = Project::new("strict");
        let mut other = Project::new("other");
        let seq = other.new_sequence("missing", Default::default(), 1, 0, None);
        let error = preflight_frame_required(&project, seq, Tick::ZERO, RenderOptions::default()).err().unwrap();
        assert_eq!(error.item_id, Some(seq));
        assert!(matches!(error.reason, EligibilityFailure::UnsupportedFrameStructure { .. }));
    }

    #[test]
    fn disabled_unknown_ordinary_effect_is_not_preflighted() {
        let mut project = Project::new("strict");
        let seq_id = project.new_sequence("sequence", Default::default(), 1, 0, None);
        let graphic = project.add_item(
            "graphic",
            filmcraft_project::Label::Iris,
            ItemKind::Graphic { width: 1920, height: 1080, rate: filmcraft_time::FrameRate::FPS_24 },
            None,
        );
        let mut item = project
            .make_track_item(
                graphic,
                filmcraft_project::TrackKind::Video,
                Tick::ZERO,
                filmcraft_time::TimeRange::new(Tick::ZERO, Tick(1_000_000)),
                filmcraft_time::FrameRate::FPS_24,
            )
            .unwrap();
        let mut effect = filmcraft_project::find_effect("brightness_contrast").unwrap().instance();
        effect.effect = "stale-ordinary-effect".into();
        item.effects.push(effect);
        let sequence = project.sequence(seq_id).unwrap();

        let error =
            preflight_item(&project, sequence, &item, Tick::ZERO, RenderOptions { effects: false, ..Default::default() }, None, &mut Vec::new()).err().unwrap();
        assert!(!matches!(error.reason, EligibilityFailure::UnknownId { .. }));
        assert!(matches!(error.reason, EligibilityFailure::CpuFallbackRequired { .. }));
    }

    #[test]
    fn required_preflight_rejects_graphics_before_materialization() {
        let mut project = Project::new("strict");
        let seq = project.new_sequence("sequence", Default::default(), 1, 0, None);
        let graphic = project.add_item(
            "graphic",
            filmcraft_project::Label::Iris,
            ItemKind::Graphic { width: 1920, height: 1080, rate: filmcraft_time::FrameRate::FPS_24 },
            None,
        );
        let item = project
            .make_track_item(
                graphic,
                filmcraft_project::TrackKind::Video,
                Tick::ZERO,
                filmcraft_time::TimeRange::new(Tick::ZERO, Tick(1_000_000)),
                filmcraft_time::FrameRate::FPS_24,
            )
            .unwrap();
        project.sequence_mut(seq).unwrap().video_tracks[0].items.push(item);
        let error = preflight_frame_required(&project, seq, Tick::ZERO, RenderOptions::default()).err().unwrap();
        assert_eq!(error.item_id, Some(graphic));
        assert!(matches!(error.reason, EligibilityFailure::CpuFallbackRequired { .. }));
    }

    #[test]
    fn flattening_an_operation_preserves_its_picture() {
        let frame = Arc::new(VideoFrame::rgba_f32(1, 1, vec![0.25, 0.0, 0.0, 0.5]));
        let steps = vec![PlanStep::Transition {
            inputs: [vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)], vec![]],
            effect: filmcraft_project::find_effect("push").unwrap().instance(),
            progress: 0.0,
            scale: 1.0,
        }];
        let expected = execute_cpu(&FramePlan::Composite { width: 1, height: 1, steps: steps.clone() });
        let layers = PlanStack { steps, exhausted: false, layers: 1 }.into_layers(1, 1).unwrap();
        let got = execute_cpu(&FramePlan::Layers { width: 1, height: 1, layers });
        assert_eq!(got.px, expected.px);
        assert!(got.px.iter().any(|v| *v != 0.0));
    }

    #[test]
    fn layer_plan_has_a_global_budget() {
        let frame = Arc::new(VideoFrame::rgba_f32(1, 1, vec![0.0; 4]));
        let mut stack = PlanStack::default();
        for _ in 0..=MAX_PLAN_LAYERS {
            stack.push(PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal));
        }
        assert!(stack.exhausted);
        assert_eq!(stack.layer_count(), MAX_PLAN_LAYERS);
    }
}
