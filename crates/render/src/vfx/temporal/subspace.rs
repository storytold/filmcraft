//! Paper-driven low-rank trajectory factorisation and motion planning.
//!
//! A supported shot is represented by one or more independent moving rank-nine runs.  Each run
//! preserves the overlapping C/E factors from Liu et al. (2011), while a temporary support gap or
//! a newly observable rank starts a bounded fresh run.  Coefficients are consequently selected by
//! source frame rather than being shared across incompatible bases.

use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use super::numerics::{solve, symmetric_eigen};
use super::trajectory_data::{FeatureTrajectory, MAX_ACTIVE_TRACKS, MAX_COORDINATE, MAX_FRAMES, MIN_TRACK_LENGTH, SUBSPACE_RANK, TrajectorySet};
use super::trajectory_index::TrajectoryIndex;

const WINDOW_FRAMES: usize = 50;
const WINDOW_STRIDE: usize = 5;
/// A rank-nine temporal basis needs nine samples; the independent persistent-track support
/// requirement below applies to trajectory rows, not the number of frames.
const MIN_WINDOW_FRAMES: usize = SUBSPACE_RANK;
const MIN_COMPLETE_TRACKS: usize = SUBSPACE_RANK.div_ceil(2);
const TARGET_COMPLETE_TRACKS: usize = SUBSPACE_RANK.saturating_mul(2);
const FACTOR_RESIDUAL_LIMIT: f64 = 3.0;
const RANK_EPSILON: f64 = 1.0e-12;
const RIDGE_EPSILON: f64 = 1.0e-10;
const MAX_GAUSSIAN_RADIUS: usize = 300;
/// A transition is accepted only when the shared-feature rows remain close to their requested
/// positions.  A rank-growth restart can leave the new row space unable to reproduce the old
/// smoothed endpoint; silently accepting that least-squares residual would turn an ill-conditioned
/// solve into a visible jump.
const TRANSITION_RESIDUAL_LIMIT: f64 = FACTOR_RESIDUAL_LIMIT;
/// Each anchor row competes against that track-dimension's `span` worth of per-frame Eq9 data
/// rows, so the multiplier is applied per shared (track, dimension) row: at 100x the data weight
/// the recovered constant stays within ridge epsilon of the previous run's planned position while
/// remaining a well-posed solve rather than a hard equality.
const NO_MOTION_ANCHOR_MULTIPLIER: f64 = 100.0;

/// Moving-factorisation failure categories. The reason strings are stable cache/UI codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubspaceError {
    Cancelled,
    InvalidInput,
    InsufficientTracks,
    NumericalFailure,
}

impl SubspaceError {
    pub(super) const fn reason(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::InvalidInput => "invalidInput",
            Self::InsufficientTracks => "insufficientPersistentTracks",
            Self::NumericalFailure => "numericalFailure",
        }
    }
}

#[derive(Clone, Debug)]
struct RunData {
    range: Range<usize>,
    /// Sparse sorted `(track id, [x/y coefficients])` rows for this independent basis.
    coefficients: Vec<(u32, [[f64; SUBSPACE_RANK]; 2])>,
}
#[derive(Clone, Debug)]
struct NoMotionAnchor {
    old_frame: usize,
    previous_basis: [f64; SUBSPACE_RANK],
    shared_tracks: Vec<u32>,
}
/// Coefficient rows selected at a supported restart boundary.  The rows belong to the new run's
/// fixed gauge and remain valid for the staged bridge even after their source observations expire;
/// Liu et al. permit the basis completion to outlive an individual trajectory.
#[derive(Clone, Copy, Debug)]
struct TransitionCoefficients {
    old: [[f64; SUBSPACE_RANK]; 2],
    new: [[f64; SUBSPACE_RANK]; 2],
}

#[derive(Debug)]
struct RunWork {
    start: usize,
    end: usize,
    coefficients: Vec<(u32, [[f64; SUBSPACE_RANK]; 2])>,
    previous: Option<WindowState>,
}

/// Shared eigen-trajectories, per-run feature coefficients, and valid discontinuity-separated
/// ranges. `basis[t][r]` is the r-th eigen-trajectory value at frame t; a coefficient row is the
/// x or y linear combination for one feature.
#[derive(Clone, Debug)]
pub(super) struct SubspaceModel {
    pub(super) basis: Vec<Option<[f64; SUBSPACE_RANK]>>,
    pub(super) segments: Vec<Range<usize>>,
    runs: Vec<RunData>,
    index: Arc<TrajectoryIndex>,
}

impl SubspaceModel {
    /// Return the coefficient row belonging to the independent run covering `frame`.
    pub(super) fn coefficient(&self, frame: usize, track: usize) -> Option<&[[f64; SUBSPACE_RANK]; 2]> {
        let run = self
            .runs
            .binary_search_by(|run| {
                if frame < run.range.start {
                    Ordering::Greater
                } else if frame >= run.range.end {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .ok()
            .and_then(|position| self.runs.get(position))?;
        let track = u32::try_from(track).ok()?;
        let position = run.coefficients.binary_search_by_key(&track, |entry| entry.0).ok()?;
        run.coefficients.get(position).map(|entry| &entry.1)
    }

    /// Return frame-local active IDs from the shared CSR observation index.
    #[inline]
    pub(super) fn active_tracks(&self, frame: usize) -> &[u32] {
        self.index.at(frame)
    }
}

#[derive(Clone, Debug)]
struct WindowState {
    start: usize,
    end: usize,
    complete_tracks: Vec<usize>,
}

/// Factor all usable trajectories into moving rank-nine subspaces.
pub(super) fn factorize(data: &TrajectorySet, progress: &mut dyn FnMut(usize)) -> Result<SubspaceModel, SubspaceError> {
    let segments = validate_data(data)?;
    if data.frames == 0 || data.tracks.is_empty() {
        return Err(SubspaceError::InsufficientTracks);
    }
    if cancelled() {
        return Err(SubspaceError::Cancelled);
    }

    let mut basis = vec![None; data.frames];
    let mut runs = Vec::new();
    let mut factored_any = false;

    for segment in &segments {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        if segment.end.saturating_sub(segment.start) < MIN_WINDOW_FRAMES {
            continue;
        }

        let mut cursor = segment.start;
        let mut current: Option<RunWork> = None;
        while cursor < segment.end {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some((window_len, complete_tracks)) = choose_window(data, segment, cursor) else {
                let next_cursor = if let Some(run) = current.take() {
                    let run_end = run.end;
                    finish_run(run, run_end, &mut runs);
                    cursor.max(run_end)
                } else {
                    cursor.saturating_add(WINDOW_STRIDE)
                };
                cursor = next_cursor;
                continue;
            };
            let Some(end) = cursor.checked_add(window_len) else {
                return Err(SubspaceError::InvalidInput);
            };
            if end <= cursor || end > segment.end || complete_tracks.len() < MIN_COMPLETE_TRACKS {
                let next_cursor = if let Some(run) = current.take() {
                    let run_end = run.end;
                    finish_run(run, run_end, &mut runs);
                    cursor.max(run_end)
                } else {
                    cursor.saturating_add(WINDOW_STRIDE)
                };
                cursor = next_cursor;
                continue;
            }

            // A failed moving update means that the old basis cannot express the newly observed
            // rank. Retire it at this cursor and initialise a fresh run without discarding later
            // usable source frames.
            let can_move = current.as_ref().is_some_and(|run| run.previous.is_some() && cursor < run.end);
            if can_move {
                let move_result = {
                    let Some(run) = current.as_mut() else { return Err(SubspaceError::NumericalFailure) };
                    let Some(previous) = run.previous.as_ref() else { return Err(SubspaceError::NumericalFailure) };
                    let result = move_window(data, previous, cursor, end, &complete_tracks, &mut basis, &mut run.coefficients);
                    match result {
                        Ok(state) => match window_residual_exceeds(data, &basis, &state, &run.coefficients) {
                            Ok(true) => Err(SubspaceError::NumericalFailure),
                            Ok(false) => Ok(state),
                            Err(error) => Err(error),
                        },
                        Err(error) => Err(error),
                    }
                };
                match move_result {
                    Ok(state) => {
                        if let Some(run) = current.as_mut() {
                            run.end = run.end.max(end);
                            run.previous = Some(state);
                        }
                        progress(end.min(data.frames));
                        cursor = cursor.saturating_add(WINDOW_STRIDE);
                        continue;
                    }
                    Err(SubspaceError::Cancelled) => return Err(SubspaceError::Cancelled),
                    Err(SubspaceError::InvalidInput) => return Err(SubspaceError::InvalidInput),
                    Err(SubspaceError::InsufficientTracks | SubspaceError::NumericalFailure) => {
                        if let Some(run) = current.take() {
                            clear_basis(&mut basis, cursor.min(run.end)..run.end);
                            finish_run(run, cursor, &mut runs);
                        }
                    }
                }
            } else if let Some(run) = current.take() {
                let run_end = run.end;
                finish_run(run, run_end, &mut runs);
            }

            let mut run = RunWork { start: cursor, end: cursor, coefficients: Vec::new(), previous: None };
            match initial_window(data, cursor, end, &complete_tracks, &mut basis, &mut run.coefficients) {
                Ok(state) => {
                    run.end = end;
                    run.previous = Some(state);
                    current = Some(run);
                    factored_any = true;
                    progress(end.min(data.frames));
                }
                Err(SubspaceError::Cancelled) => return Err(SubspaceError::Cancelled),
                Err(SubspaceError::InvalidInput) => return Err(SubspaceError::InvalidInput),
                Err(SubspaceError::InsufficientTracks) => {}
                Err(SubspaceError::NumericalFailure) => {}
            }
            cursor = cursor.saturating_add(WINDOW_STRIDE);
        }
        if let Some(run) = current.take() {
            let run_end = run.end;
            finish_run(run, run_end, &mut runs);
        }
    }

    if !factored_any || runs.is_empty() {
        return Err(SubspaceError::InsufficientTracks);
    }
    runs.sort_by_key(|run| run.range.start);
    let mut retained_end = 0usize;
    runs.retain(|run| {
        if run.range.start < retained_end {
            return false;
        }
        retained_end = run.range.end;
        true
    });
    // Recovery boundaries are cut before a new run starts; retain the first range defensively if
    // malformed state ever leaves an overlap.
    let mut segments_out = Vec::with_capacity(runs.len());
    let mut previous_end = 0usize;
    for run in &runs {
        if run.range.start >= run.range.end || run.range.end > data.frames {
            continue;
        }
        if run.range.start < previous_end {
            continue;
        }
        segments_out.push(run.range.clone());
        previous_end = run.range.end;
    }
    if segments_out.is_empty() {
        return Err(SubspaceError::InsufficientTracks);
    }

    project_unassigned(data, &basis, &mut runs)?;
    reject_factorization_outliers(data, &basis, &mut runs)?;
    runs.retain(|run| run.range.start < run.range.end);
    segments_out = runs.iter().map(|run| run.range.clone()).collect();
    if segments_out.is_empty() {
        return Err(SubspaceError::InsufficientTracks);
    }

    if cancelled() {
        return Err(SubspaceError::Cancelled);
    }
    Ok(SubspaceModel { basis, segments: segments_out, runs, index: Arc::clone(&data.index) })
}

/// Smooth common eigen-trajectories and return the planned per-frame basis.
pub(super) fn planned_basis(
    data: &TrajectorySet,
    model: &SubspaceModel,
    smooth_frames: f64,
    no_motion: bool,
) -> Result<Vec<Option<[f64; SUBSPACE_RANK]>>, SubspaceError> {
    let data_segments = validate_data(data)?;
    validate_model(data, model)?;
    if !smooth_frames.is_finite() || smooth_frames < 0.0 {
        return Err(SubspaceError::InvalidInput);
    }
    if cancelled() {
        return Err(SubspaceError::Cancelled);
    }

    let mut planned = vec![None; data.frames];
    let segments = if model.segments.is_empty() { data_segments } else { model.segments.clone() };
    if no_motion {
        let mut previous_constant: Option<(Range<usize>, [f64; SUBSPACE_RANK])> = None;
        for segment in &segments {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let anchor = previous_constant
                .as_ref()
                .and_then(|(previous_segment, previous_basis)| no_motion_anchor(data, model, previous_segment, *previous_basis, segment));
            let constant = fit_constant_basis(data, model, segment, anchor.as_ref())?;
            for frame in segment.clone() {
                if cancelled() {
                    return Err(SubspaceError::Cancelled);
                }
                if frame >= data.frames {
                    return Err(SubspaceError::InvalidInput);
                }
                if model.basis.get(frame).is_some_and(|value| value.is_some()) {
                    planned[frame] = constant;
                }
            }
            if let Some(value) = constant {
                previous_constant = Some((segment.clone(), value));
            } else {
                previous_constant = None;
            }
        }
        return Ok(planned);
    }

    let radius = (smooth_frames * (10.0 / 3.0)).ceil().min(MAX_GAUSSIAN_RADIUS as f64) as usize;
    if radius == 0 {
        return Ok(model.basis.clone());
    }
    let sigma = (radius as f64 / 2.0_f64.sqrt()).max(1.0e-9);
    let radius_i = radius as isize;
    for segment in &segments {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        if segment.start >= segment.end || segment.end > data.frames {
            return Err(SubspaceError::InvalidInput);
        }
        for frame in segment.clone() {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            if model.basis.get(frame).and_then(|basis| *basis).is_none() {
                continue;
            }
            let mut sum = [0.0f64; SUBSPACE_RANK];
            let mut weight_sum = 0.0f64;
            let lo = frame.saturating_sub(radius);
            let hi = frame.saturating_add(radius).min(segment.end.saturating_sub(1));
            for neighbour in lo.max(segment.start)..=hi {
                if cancelled() {
                    return Err(SubspaceError::Cancelled);
                }
                let Some(value) = model.basis.get(neighbour).and_then(|basis| *basis) else { continue };
                let distance = neighbour as isize - frame as isize;
                if distance.abs() > radius_i {
                    continue;
                }
                let d = distance as f64;
                let weight = (-0.5 * (d / sigma) * (d / sigma)).exp();
                if !weight.is_finite() {
                    return Err(SubspaceError::NumericalFailure);
                }
                weight_sum += weight;
                if !weight_sum.is_finite() {
                    return Err(SubspaceError::NumericalFailure);
                }
                for component in 0..SUBSPACE_RANK {
                    sum[component] += weight * value[component];
                    if !sum[component].is_finite() {
                        return Err(SubspaceError::NumericalFailure);
                    }
                }
            }
            if weight_sum > f64::MIN_POSITIVE && weight_sum.is_finite() {
                let mut value = [0.0f64; SUBSPACE_RANK];
                for component in 0..SUBSPACE_RANK {
                    value[component] = sum[component] / weight_sum;
                    if !value[component].is_finite() {
                        return Err(SubspaceError::NumericalFailure);
                    }
                }
                planned[frame] = Some(value);
            }
        }
    }

    // A moving-factorisation restart changes the basis gauge and can also expose a new rank.  The
    // Gaussian above is intentionally local to each run, but its edge normalisation would
    // otherwise turn a supported restart into a feature step.  Bridge only contiguous runs with
    // shared observations and no declared source cut.  Each bridge value is one rank-nine basis
    // solved from all shared feature rows at once; no sparse track is filtered independently.
    bridge_supported_boundaries(data, model, &segments, radius, &mut planned)?;
    if cancelled() {
        return Err(SubspaceError::Cancelled);
    }
    Ok(planned)
}

fn bridge_supported_boundaries(
    data: &TrajectorySet,
    model: &SubspaceModel,
    segments: &[Range<usize>],
    radius: usize,
    planned: &mut [Option<[f64; SUBSPACE_RANK]>],
) -> Result<(), SubspaceError> {
    for pair in segments.windows(2) {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let Some(previous) = pair.first() else { continue };
        let Some(current) = pair.get(1) else { continue };
        // A declared source cut or a gap in supported basis coverage stays an honest boundary.
        if previous.end != current.start {
            continue;
        }
        if data.segment_starts.binary_search(&current.start).is_ok() {
            continue;
        }
        let old_frame = previous.end.saturating_sub(1);
        let boundary = current.start;
        let Some(old_basis) = planned.get(old_frame).and_then(|value| *value) else {
            continue;
        };
        let shared = shared_boundary_tracks(data, model, old_frame, boundary)?;
        if shared.len() < MIN_COMPLETE_TRACKS {
            continue;
        }
        let Some(shared_coefficients) = capture_transition_coefficients(model, old_frame, boundary, &shared) else {
            continue;
        };
        let bridge_len = radius.min(current.end.saturating_sub(boundary).saturating_sub(1));
        // Residual gate floor, widened only by the discontinuity this boundary would otherwise
        // expose verbatim: accepting a fit must beat skipping by at least 2x at the boundary.
        let Some(entry_basis) = planned.get(boundary).and_then(|value| *value) else {
            continue;
        };
        let allowance = transition_allowance(old_basis, entry_basis, &shared_coefficients);
        let mut transition = Vec::with_capacity(bridge_len.saturating_add(1));
        let mut valid = true;
        let mut fit_failed = false;
        for frame in boundary..=boundary.saturating_add(bridge_len) {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(new_basis) = planned.get(frame).and_then(|value| *value) else {
                valid = false;
                break;
            };
            let alpha = smoothstep(frame.saturating_sub(boundary), bridge_len);
            let Some(value) = fit_transition_basis(old_basis, new_basis, &shared_coefficients, alpha, allowance) else {
                valid = false;
                fit_failed = true;
                break;
            };
            transition.push((frame, value));
        }
        if valid {
            for (frame, value) in transition {
                if let Some(slot) = planned.get_mut(frame) {
                    *slot = Some(value);
                }
            }
            continue;
        }
        if !fit_failed {
            continue;
        }

        // A fresh rank can make the old *smoothed* endpoint genuinely unreachable in the new
        // coefficient row space.  Do not widen the residual bound or publish a partial solve:
        // taper the edge-normalised old basis back to its native endpoint, then enter the new
        // gauge from its native boundary value.  The old and new native factors each reconstruct
        // the observed boundary samples, while every staged value remains one rank-nine basis.
        let Some(old_native) = model.basis.get(old_frame).and_then(|value| *value) else {
            continue;
        };
        let Some(new_native) = model.basis.get(boundary).and_then(|value| *value) else {
            continue;
        };
        let native_allowance = transition_allowance(old_native, entry_basis, &shared_coefficients);
        let mut edge_transition = Vec::with_capacity(bridge_len.saturating_add(1));
        let mut edge_fit = true;
        for frame in boundary..=boundary.saturating_add(bridge_len) {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(new_basis) = planned.get(frame).and_then(|value| *value) else {
                edge_fit = false;
                break;
            };
            let alpha = smoothstep(frame.saturating_sub(boundary), bridge_len);
            let Some(value) = fit_transition_basis(old_native, new_basis, &shared_coefficients, alpha, native_allowance) else {
                edge_fit = false;
                break;
            };
            edge_transition.push((frame, value));
        }
        if !edge_fit {
            edge_transition.clear();
            for frame in boundary..=boundary.saturating_add(bridge_len) {
                if cancelled() {
                    return Err(SubspaceError::Cancelled);
                }
                let Some(new_basis) = planned.get(frame).and_then(|value| *value) else {
                    edge_fit = false;
                    break;
                };
                let alpha = smoothstep(frame.saturating_sub(boundary), bridge_len);
                let Some(value) = blend_basis(new_native, new_basis, alpha) else {
                    edge_fit = false;
                    break;
                };
                edge_transition.push((frame, value));
            }
        }
        if !edge_fit {
            continue;
        }

        // Keep the old-edge taper disjoint from the previous boundary's first half.  Adjacent
        // short runs may overlap in the Gaussian support; assigning only this suffix prevents a
        // later incompatible restart from rewriting the already committed boundary entry.
        let suffix_len = radius.min(previous.end.saturating_sub(previous.start).saturating_div(2).max(1));
        let suffix_start = previous.end.saturating_sub(suffix_len);
        let mut edge = Vec::with_capacity(suffix_len.saturating_add(edge_transition.len()));
        let suffix_span = old_frame.saturating_sub(suffix_start);
        for frame in suffix_start..=old_frame {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(existing) = planned.get(frame).and_then(|value| *value) else {
                edge_fit = false;
                break;
            };
            let alpha = smoothstep(frame.saturating_sub(suffix_start), suffix_span);
            let Some(value) = blend_basis(existing, old_native, alpha) else {
                edge_fit = false;
                break;
            };
            edge.push((frame, value));
        }
        if !edge_fit {
            continue;
        }
        edge.extend(edge_transition);
        // Publish the complete edge transition only after both gauges have a bounded value.
        for (frame, value) in edge {
            if let Some(slot) = planned.get_mut(frame) {
                *slot = Some(value);
            }
        }
    }
    Ok(())
}

#[inline]
fn smoothstep(frame_offset: usize, span: usize) -> f64 {
    if span == 0 {
        return 1.0;
    }
    let u = (frame_offset as f64 / span as f64).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

fn blend_basis(from: [f64; SUBSPACE_RANK], to: [f64; SUBSPACE_RANK], alpha: f64) -> Option<[f64; SUBSPACE_RANK]> {
    if !alpha.is_finite() || !from.iter().chain(to.iter()).all(|value| value.is_finite()) {
        return None;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let mut value = [0.0f64; SUBSPACE_RANK];
    for component in 0..SUBSPACE_RANK {
        value[component] = from[component] + (to[component] - from[component]) * alpha;
        if !value[component].is_finite() || value[component].abs() > f64::from(MAX_COORDINATE) {
            return None;
        }
    }
    Some(value)
}

/// Tracks observable in both runs' boundary frames with coefficients in each run's own gauge.
fn shared_boundary_tracks(data: &TrajectorySet, model: &SubspaceModel, old_frame: usize, new_frame: usize) -> Result<Vec<u32>, SubspaceError> {
    let old_ids = model.active_tracks(old_frame);
    let new_ids = model.active_tracks(new_frame);
    let mut shared = Vec::with_capacity(old_ids.len().min(new_ids.len()).min(MAX_ACTIVE_TRACKS));
    for &id in old_ids {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        if new_ids.binary_search(&id).is_err() {
            continue;
        }
        let Some(index) = usize::try_from(id).ok() else { continue };
        let Some(track) = data.tracks.get(index) else { continue };
        if track.rejected || model.coefficient(old_frame, index).is_none() || model.coefficient(new_frame, index).is_none() {
            continue;
        }
        shared.push(id);
        if shared.len() >= MAX_ACTIVE_TRACKS {
            break;
        }
    }
    Ok(shared)
}

/// Capture the old-run and new-run coefficient rows once at the bridge boundary.  A trajectory
/// may stop being observable during the staged interval, but its selected new-run row still
/// constrains the basis completion for the whole bridge.
fn capture_transition_coefficients(model: &SubspaceModel, old_frame: usize, new_frame: usize, shared: &[u32]) -> Option<Vec<TransitionCoefficients>> {
    let mut coefficients = Vec::with_capacity(shared.len());
    for &id in shared {
        let index = usize::try_from(id).ok()?;
        let old = *model.coefficient(old_frame, index)?;
        let new = *model.coefficient(new_frame, index)?;
        coefficients.push(TransitionCoefficients { old, new });
    }
    (coefficients.len() >= MIN_COMPLETE_TRACKS).then_some(coefficients)
}

/// Solve one rank-nine basis that reproduces a smooth per-feature blend at a supported restart.
///
/// The blend target moves each shared feature's planned position from its value under the
/// previous run's gauge (`old_basis` at `old_frame`) to its value under the new run's gauge
/// (`new_basis` at the boundary) with a smoothstep weight `alpha`, all expressed in the new
/// run's coefficient rows captured at that boundary.  One linear solve covers every shared
/// feature simultaneously, so this is a gauge-consistent transition of the whole subspace, not
/// per-track filtering.
///
/// No basis-to-basis inverse is formed here.  A rank-growth restart may leave one of the old
/// nine-dimensional directions absent from the new row space; the regularised row solve remains
/// valid in that case, and the residual check below rejects only a transition that could not
/// reproduce the requested shared-feature positions.
fn fit_transition_basis(
    old_basis: [f64; SUBSPACE_RANK],
    new_basis: [f64; SUBSPACE_RANK],
    shared: &[TransitionCoefficients],
    alpha: f64,
    residual_limit: f64,
) -> Option<[f64; SUBSPACE_RANK]> {
    if !alpha.is_finite() || !old_basis.iter().all(|value| value.is_finite()) || !new_basis.iter().all(|value| value.is_finite()) {
        return None;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let mut normal = [[0.0f64; SUBSPACE_RANK]; SUBSPACE_RANK];
    let mut rhs = [0.0f64; SUBSPACE_RANK];
    let mut rows = 0usize;
    for coefficients in shared {
        if cancelled() {
            return None;
        }
        let old_coefficients = &coefficients.old;
        let new_coefficients = &coefficients.new;
        for dimension in 0..2 {
            let old_target = dot(&old_coefficients[dimension], &old_basis);
            let new_target = dot(&new_coefficients[dimension], &new_basis);
            let target = if alpha >= 1.0 { new_target } else { old_target + (new_target - old_target) * alpha };
            if !old_target.is_finite() || !new_target.is_finite() || !target.is_finite() {
                return None;
            }
            // Solve the transition as a correction to the native new-run basis.  Shared rows can
            // be rank deficient, so a zero-centred solve would erase new-run components that
            // those rows cannot observe even when alpha reaches the exact native endpoint.
            let correction = target - new_target;
            if !correction.is_finite() {
                return None;
            }
            let row = new_coefficients[dimension];
            for i in 0..SUBSPACE_RANK {
                for j in 0..SUBSPACE_RANK {
                    normal[i][j] += row[i] * row[j];
                }
                rhs[i] += row[i] * correction;
            }
            rows = rows.saturating_add(1);
        }
    }
    if rows < MIN_COMPLETE_TRACKS.saturating_mul(2) || normal.iter().flatten().any(|value| !value.is_finite()) || rhs.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let delta = solve_regularized(normal, rhs)?;
    let mut value = [0.0f64; SUBSPACE_RANK];
    for component in 0..SUBSPACE_RANK {
        value[component] = new_basis[component] + delta[component];
        if !value[component].is_finite() || value[component].abs() > f64::from(MAX_COORDINATE) {
            return None;
        }
    }
    if alpha >= 1.0 {
        value = new_basis;
    }
    if !value.iter().all(|component| component.is_finite() && component.abs() <= f64::from(MAX_COORDINATE)) {
        return None;
    }

    // A ridge solve is intentionally allowed for rank-deficient coefficient rows, but it must
    // still explain the requested common-subspace targets.  Rejecting a large residual avoids
    // replacing a visible restart with an arbitrary minimum-norm direction.
    let mut max_residual = 0.0_f64;
    for coefficients in shared {
        if cancelled() {
            return None;
        }
        let old_coefficients = &coefficients.old;
        let new_coefficients = &coefficients.new;
        for dimension in 0..2 {
            let old_target = dot(&old_coefficients[dimension], &old_basis);
            let new_target = dot(&new_coefficients[dimension], &new_basis);
            let target = if alpha >= 1.0 { new_target } else { old_target + (new_target - old_target) * alpha };
            let reconstructed = dot(&new_coefficients[dimension], &value);
            let residual = (reconstructed - target).abs();
            if !target.is_finite() || !reconstructed.is_finite() || !residual.is_finite() {
                return None;
            }
            max_residual = max_residual.max(residual);
        }
    }
    if !residual_limit.is_finite() || residual_limit < 0.0 {
        return None;
    }
    (max_residual <= residual_limit).then_some(value)
}

/// Per-boundary residual gate.  The floor stays `TRANSITION_RESIDUAL_LIMIT`; it widens only by
/// half the shared-feature discontinuity the two gauges would otherwise expose as a single-frame
/// step, so a transition is accepted only when its worst row residual clearly beats skipping.
fn transition_allowance(old_basis: [f64; SUBSPACE_RANK], new_basis: [f64; SUBSPACE_RANK], shared: &[TransitionCoefficients]) -> f64 {
    let mut discontinuity = 0.0_f64;
    for coefficients in shared {
        let old_coefficients = &coefficients.old;
        let new_coefficients = &coefficients.new;
        for dimension in 0..2 {
            let old_target = dot(&old_coefficients[dimension], &old_basis);
            let new_target = dot(&new_coefficients[dimension], &new_basis);
            if !old_target.is_finite() || !new_target.is_finite() {
                return TRANSITION_RESIDUAL_LIMIT;
            }
            discontinuity = discontinuity.max((old_target - new_target).abs());
        }
    }
    TRANSITION_RESIDUAL_LIMIT.max(0.5 * discontinuity)
}

fn validate_data(data: &TrajectorySet) -> Result<Vec<Range<usize>>, SubspaceError> {
    if data.width == 0 || data.height == 0 || data.frames > MAX_FRAMES || data.index.frames() != data.frames {
        return Err(SubspaceError::InvalidInput);
    }
    if data.width > 100_000_000 || data.height > 100_000_000 {
        return Err(SubspaceError::InvalidInput);
    }
    if !data.saliency.is_empty() && data.saliency.len() != data.frames {
        return Err(SubspaceError::InvalidInput);
    }
    if data.saliency.iter().flatten().any(|value| !value.is_finite()) {
        return Err(SubspaceError::InvalidInput);
    }
    if data.tracks.len() > data.frames.saturating_mul(MAX_ACTIVE_TRACKS) {
        return Err(SubspaceError::InvalidInput);
    }
    if data.segment_starts.len() > data.frames.saturating_add(1) {
        return Err(SubspaceError::InvalidInput);
    }
    if data.frames == 0 {
        if data.segment_starts.iter().any(|start| *start != 0) || !data.tracks.is_empty() {
            return Err(SubspaceError::InvalidInput);
        }
        return Ok(Vec::new());
    }
    if data.segment_starts.first().copied().is_some_and(|start| start != 0) {
        return Err(SubspaceError::InvalidInput);
    }
    for pair in data.segment_starts.windows(2) {
        if pair[0] >= pair[1] || pair[1] > data.frames {
            return Err(SubspaceError::InvalidInput);
        }
    }
    let mut starts = if data.segment_starts.is_empty() { vec![0] } else { data.segment_starts.clone() };
    if starts.last().copied() == Some(data.frames) {
        starts.pop();
    }
    let mut segments = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(data.frames);
        if start >= end || end > data.frames {
            return Err(SubspaceError::InvalidInput);
        }
        segments.push(start..end);
    }

    let max_observations = data.frames.saturating_mul(MAX_ACTIVE_TRACKS);
    let mut observations = 0usize;
    for track in &data.tracks {
        if track.start > data.frames || track.points.iter().flatten().any(|value| !value.is_finite() || f64::from(value.abs()) > f64::from(MAX_COORDINATE)) {
            return Err(SubspaceError::InvalidInput);
        }
        let end = track.start.checked_add(track.points.len()).ok_or(SubspaceError::InvalidInput)?;
        if end > data.frames {
            return Err(SubspaceError::InvalidInput);
        }
        observations = observations.checked_add(track.points.len()).ok_or(SubspaceError::InvalidInput)?;
        if observations > max_observations {
            return Err(SubspaceError::InvalidInput);
        }
    }
    Ok(segments)
}

fn validate_model(data: &TrajectorySet, model: &SubspaceModel) -> Result<(), SubspaceError> {
    if model.basis.len() != data.frames || model.index.frames() != data.frames || model.runs.len() != model.segments.len() {
        return Err(SubspaceError::InvalidInput);
    }
    if model.basis.iter().flatten().flatten().any(|value| !value.is_finite()) {
        return Err(SubspaceError::InvalidInput);
    }
    let mut previous_end = 0usize;
    for (run, segment) in model.runs.iter().zip(&model.segments) {
        if segment.start >= segment.end || segment.end > data.frames || segment != &run.range || segment.start < previous_end {
            return Err(SubspaceError::InvalidInput);
        }
        previous_end = segment.end;
        let mut previous_track = None;
        for (track, coefficients) in &run.coefficients {
            let Some(index) = usize::try_from(*track).ok() else {
                return Err(SubspaceError::InvalidInput);
            };
            if index >= data.tracks.len() {
                return Err(SubspaceError::InvalidInput);
            }
            if previous_track.is_some_and(|old| *track <= old) || coefficients.iter().flatten().any(|value| !value.is_finite()) {
                return Err(SubspaceError::InvalidInput);
            }
            previous_track = Some(*track);
        }
    }
    if !model.basis.iter().any(Option::is_some) {
        return Err(SubspaceError::InsufficientTracks);
    }
    Ok(())
}

fn choose_window(data: &TrajectorySet, segment: &Range<usize>, start: usize) -> Option<(usize, Vec<usize>)> {
    let remaining = segment.end.checked_sub(start)?;
    let max_len = remaining.min(WINDOW_FRAMES);
    if max_len < MIN_WINDOW_FRAMES {
        return None;
    }
    // The CSR row gives the only tracks that can be complete at this start. Sorting their end
    // times once lets shorter candidate windows admit more tracks without rescanning history.
    let mut candidates = complete_candidates(data, segment, start);
    candidates.sort_unstable_by_key(|candidate| candidate.1);
    let mut first = candidates.len();
    let mut feasible = None;
    for length in (MIN_WINDOW_FRAMES..=max_len).rev() {
        let end = start.checked_add(length)?;
        while first > 0 && candidates.get(first - 1).is_some_and(|candidate| candidate.1 >= end) {
            first -= 1;
        }
        let complete_count = candidates.len().saturating_sub(first);
        if complete_count >= TARGET_COMPLETE_TRACKS || (feasible.is_none() && complete_count >= MIN_COMPLETE_TRACKS) {
            let mut complete: Vec<usize> = candidates.get(first..)?.iter().map(|candidate| candidate.0).collect();
            complete.sort_unstable();
            if complete_count >= TARGET_COMPLETE_TRACKS {
                return Some((length, complete));
            }
            feasible = Some((length, complete));
        }
    }
    feasible
}

fn complete_candidates(data: &TrajectorySet, segment: &Range<usize>, start: usize) -> Vec<(usize, usize)> {
    let mut candidates = Vec::with_capacity(MAX_ACTIVE_TRACKS);
    for &id in data.index.at(start) {
        if cancelled() {
            break;
        }
        let Some(index) = usize::try_from(id).ok() else { continue };
        let Some(track) = data.tracks.get(index) else { continue };
        let Some(track_end) = track.start.checked_add(track.points.len()) else { continue };
        if track.rejected
            || track.points.len() < MIN_TRACK_LENGTH
            || track.start > start
            || track.start < segment.start
            || track_end > segment.end
            || complete_track_crosses_cut(data, track)
        {
            continue;
        }
        candidates.push((index, track_end));
    }
    candidates
}

fn complete_track_crosses_cut(data: &TrajectorySet, track: &FeatureTrajectory) -> bool {
    let Some(end) = track.start.checked_add(track.points.len()) else { return true };
    data.segment_starts.iter().any(|cut| *cut > track.start && *cut < end)
}

fn initial_window(
    data: &TrajectorySet,
    start: usize,
    end: usize,
    complete_tracks: &[usize],
    basis: &mut [Option<[f64; SUBSPACE_RANK]>],
    coefficients: &mut Vec<(u32, [[f64; SUBSPACE_RANK]; 2])>,
) -> Result<WindowState, SubspaceError> {
    let matrix = assemble_matrix(data, start, end, complete_tracks)?;
    let (window_basis, window_coefficients) = truncated_svd(&matrix, complete_tracks.len(), end - start)?;
    for (offset, value) in window_basis.iter().copied().enumerate() {
        let frame = start.checked_add(offset).ok_or(SubspaceError::InvalidInput)?;
        let Some(slot) = basis.get_mut(frame) else { return Err(SubspaceError::InvalidInput) };
        *slot = Some(value);
    }
    for (row, track_index) in complete_tracks.iter().copied().enumerate() {
        let track = u32::try_from(track_index).map_err(|_| SubspaceError::InvalidInput)?;
        let Some(value) = window_coefficients.get(row).copied() else { return Err(SubspaceError::NumericalFailure) };
        set_coefficient(coefficients, track, value);
    }
    Ok(WindowState { start, end, complete_tracks: complete_tracks.to_vec() })
}

fn move_window(
    data: &TrajectorySet,
    previous: &WindowState,
    start: usize,
    end: usize,
    complete_tracks: &[usize],
    basis: &mut [Option<[f64; SUBSPACE_RANK]>],
    coefficients: &mut Vec<(u32, [[f64; SUBSPACE_RANK]; 2])>,
) -> Result<WindowState, SubspaceError> {
    let overlap_end = previous.end.min(end);
    if start >= overlap_end || start < previous.start {
        return Err(SubspaceError::NumericalFailure);
    }
    let overlap_len = overlap_end - start;
    let mut overlap_basis = Vec::with_capacity(overlap_len);
    for frame in start..overlap_end {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let Some(value) = basis.get(frame).and_then(|value| *value) else { return Err(SubspaceError::NumericalFailure) };
        overlap_basis.push(value);
    }

    let shared: Vec<usize> = previous
        .complete_tracks
        .iter()
        .copied()
        .filter(|index| complete_tracks.binary_search(index).is_ok() && coefficient_for(coefficients, *index as u32).is_some())
        .collect();
    let new_tracks: Vec<usize> = complete_tracks.iter().copied().filter(|index| shared.binary_search(index).is_err()).collect();

    // Eq. 7: project each newly visible complete trajectory onto fixed overlapping E¹.
    let gram = basis_gram(&overlap_basis);
    for track_index in new_tracks.iter().copied() {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let Some(track) = data.tracks.get(track_index) else { return Err(SubspaceError::InvalidInput) };
        let mut rows = [[0.0f64; SUBSPACE_RANK]; 2];
        for dimension in 0..2 {
            let mut rhs = [0.0f64; SUBSPACE_RANK];
            for (offset, frame) in (start..overlap_end).enumerate() {
                if cancelled() {
                    return Err(SubspaceError::Cancelled);
                }
                let Some(point) = track.points.get(frame.checked_sub(track.start).ok_or(SubspaceError::InvalidInput)?) else {
                    return Err(SubspaceError::InvalidInput);
                };
                let value = point[dimension] as f64;
                let Some(eigen) = overlap_basis.get(offset) else { return Err(SubspaceError::NumericalFailure) };
                for component in 0..SUBSPACE_RANK {
                    rhs[component] += value * eigen[component];
                    if !rhs[component].is_finite() {
                        return Err(SubspaceError::NumericalFailure);
                    }
                }
            }
            rows[dimension] = solve_regularized(gram, rhs).ok_or(SubspaceError::NumericalFailure)?;
        }
        let track_id = u32::try_from(track_index).map_err(|_| SubspaceError::InvalidInput)?;
        set_coefficient(coefficients, track_id, rows);
    }

    let new_start = overlap_end;
    if new_start < end {
        // Eq. 8: solve new E columns using fixed C¹ and projected C² rows.
        let mut normal = [[0.0f64; SUBSPACE_RANK]; SUBSPACE_RANK];
        let mut rhs_columns = vec![[0.0f64; SUBSPACE_RANK]; end - new_start];
        for track_index in shared.iter().chain(new_tracks.iter()) {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(coefficients_for_track) = coefficient_for(coefficients, *track_index as u32).copied() else { continue };
            let Some(track) = data.tracks.get(*track_index) else { return Err(SubspaceError::InvalidInput) };
            for dimension in 0..2 {
                let row = coefficients_for_track[dimension];
                for i in 0..SUBSPACE_RANK {
                    for j in 0..SUBSPACE_RANK {
                        normal[i][j] += row[i] * row[j];
                    }
                }
                for (offset, frame) in (new_start..end).enumerate() {
                    if cancelled() {
                        return Err(SubspaceError::Cancelled);
                    }
                    let Some(point) = track.points.get(frame.checked_sub(track.start).ok_or(SubspaceError::InvalidInput)?) else {
                        return Err(SubspaceError::InvalidInput);
                    };
                    let value = point[dimension] as f64;
                    let Some(column) = rhs_columns.get_mut(offset) else { return Err(SubspaceError::NumericalFailure) };
                    for component in 0..SUBSPACE_RANK {
                        column[component] += row[component] * value;
                    }
                }
            }
        }
        if normal.iter().flatten().any(|value| !value.is_finite()) || rhs_columns.iter().flatten().any(|value| !value.is_finite()) {
            return Err(SubspaceError::NumericalFailure);
        }
        for (offset, frame) in (new_start..end).enumerate() {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(value) = solve_regularized(normal, rhs_columns[offset]) else { return Err(SubspaceError::NumericalFailure) };
            let Some(slot) = basis.get_mut(frame) else { return Err(SubspaceError::InvalidInput) };
            *slot = Some(value);
        }
    }
    Ok(WindowState { start, end, complete_tracks: complete_tracks.to_vec() })
}

fn window_residual_exceeds(
    data: &TrajectorySet,
    basis: &[Option<[f64; SUBSPACE_RANK]>],
    state: &WindowState,
    coefficients: &[(u32, [[f64; SUBSPACE_RANK]; 2])],
) -> Result<bool, SubspaceError> {
    let check_start = state.start.max(state.end.saturating_sub(WINDOW_STRIDE));
    let mut observed = 0usize;
    let mut bad = 0usize;
    for track_index in &state.complete_tracks {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let Some(coefficients_for_track) = coefficient_for(coefficients, u32::try_from(*track_index).map_err(|_| SubspaceError::InvalidInput)?) else {
            continue;
        };
        let Some(track) = data.tracks.get(*track_index) else { return Err(SubspaceError::InvalidInput) };
        let end = state.end.min(track.start.saturating_add(track.points.len()));
        for frame in check_start..end {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(eigen) = basis.get(frame).and_then(|value| *value) else { continue };
            let Some(point) = track.points.get(frame.checked_sub(track.start).ok_or(SubspaceError::InvalidInput)?) else {
                return Err(SubspaceError::InvalidInput);
            };
            let error = (dot(&coefficients_for_track[0], &eigen) - point[0] as f64).hypot(dot(&coefficients_for_track[1], &eigen) - point[1] as f64);
            if !error.is_finite() {
                return Err(SubspaceError::NumericalFailure);
            }
            observed = observed.saturating_add(1);
            if error > FACTOR_RESIDUAL_LIMIT {
                bad = bad.saturating_add(1);
            }
        }
    }
    Ok(observed > 0 && bad.saturating_mul(3) > observed)
}

fn assemble_matrix(data: &TrajectorySet, start: usize, end: usize, track_indices: &[usize]) -> Result<Vec<Vec<f64>>, SubspaceError> {
    let columns = end.checked_sub(start).ok_or(SubspaceError::InvalidInput)?;
    if columns == 0 || columns > WINDOW_FRAMES {
        return Err(SubspaceError::InvalidInput);
    }
    let mut matrix = Vec::with_capacity(track_indices.len().saturating_mul(2));
    for track_index in track_indices.iter().copied() {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let Some(track) = data.tracks.get(track_index) else { return Err(SubspaceError::InvalidInput) };
        for dimension in 0..2 {
            let mut row = Vec::with_capacity(columns);
            for frame in start..end {
                if cancelled() {
                    return Err(SubspaceError::Cancelled);
                }
                let Some(point) = track.points.get(frame.checked_sub(track.start).ok_or(SubspaceError::InvalidInput)?) else {
                    return Err(SubspaceError::InvalidInput);
                };
                row.push(point[dimension] as f64);
            }
            matrix.push(row);
        }
    }
    Ok(matrix)
}

fn truncated_svd(
    matrix: &[Vec<f64>],
    track_count: usize,
    columns: usize,
) -> Result<(Vec<[f64; SUBSPACE_RANK]>, Vec<[[f64; SUBSPACE_RANK]; 2]>), SubspaceError> {
    if track_count == 0 || columns == 0 || columns > WINDOW_FRAMES || matrix.len() != track_count.saturating_mul(2) {
        return Err(SubspaceError::InvalidInput);
    }
    let mut gram = [[0.0f64; WINDOW_FRAMES]; WINDOW_FRAMES];
    for row in matrix {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        if row.len() != columns || row.iter().any(|value| !value.is_finite()) {
            return Err(SubspaceError::InvalidInput);
        }
        for i in 0..columns {
            for j in i..columns {
                gram[i][j] += row[i] * row[j];
                if !gram[i][j].is_finite() {
                    return Err(SubspaceError::NumericalFailure);
                }
                gram[j][i] = gram[i][j];
            }
        }
    }
    let (eigenvalues, eigenvectors) = symmetric_eigen(gram).ok_or(SubspaceError::NumericalFailure)?;
    let largest = eigenvalues.first().copied().unwrap_or(0.0).max(0.0);
    let mut window_basis = vec![[0.0f64; SUBSPACE_RANK]; columns];
    let mut window_coefficients = vec![[[0.0f64; SUBSPACE_RANK]; 2]; track_count];

    for component in 0..SUBSPACE_RANK {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let eigenvalue = eigenvalues.get(component).copied().unwrap_or(0.0);
        let singular = if eigenvalue > largest * RANK_EPSILON && eigenvalue > 0.0 { eigenvalue.sqrt() } else { 0.0 };
        if !singular.is_finite() {
            return Err(SubspaceError::NumericalFailure);
        }
        for column in 0..columns {
            let vector = eigenvectors[column][component];
            let value = singular * vector;
            if !value.is_finite() {
                return Err(SubspaceError::NumericalFailure);
            }
            window_basis[column][component] = value;
        }
        if singular > 0.0 {
            for row in 0..matrix.len() {
                let projection = matrix[row].iter().enumerate().map(|(column, value)| value * eigenvectors[column][component]).sum::<f64>();
                let coefficient = projection / singular;
                if !projection.is_finite() || !coefficient.is_finite() {
                    return Err(SubspaceError::NumericalFailure);
                }
                window_coefficients[row / 2][row % 2][component] = coefficient;
            }
        }
    }
    Ok((window_basis, window_coefficients))
}

fn basis_gram(basis: &[[f64; SUBSPACE_RANK]]) -> [[f64; SUBSPACE_RANK]; SUBSPACE_RANK] {
    let mut gram = [[0.0f64; SUBSPACE_RANK]; SUBSPACE_RANK];
    for value in basis {
        for i in 0..SUBSPACE_RANK {
            for j in 0..SUBSPACE_RANK {
                gram[i][j] += value[i] * value[j];
            }
        }
    }
    gram
}

fn solve_regularized(mut matrix: [[f64; SUBSPACE_RANK]; SUBSPACE_RANK], rhs: [f64; SUBSPACE_RANK]) -> Option<[f64; SUBSPACE_RANK]> {
    if matrix.iter().flatten().any(|value| !value.is_finite()) || rhs.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let scale = matrix.iter().flatten().map(|value| value.abs()).fold(0.0, f64::max);
    if !scale.is_finite() {
        return None;
    }
    let ridge = scale.max(1.0) * RIDGE_EPSILON;
    for i in 0..SUBSPACE_RANK {
        matrix[i][i] += ridge;
    }
    solve(matrix, rhs)
}

fn set_coefficient(entries: &mut Vec<(u32, [[f64; SUBSPACE_RANK]; 2])>, track: u32, value: [[f64; SUBSPACE_RANK]; 2]) {
    match entries.binary_search_by_key(&track, |entry| entry.0) {
        Ok(position) => {
            if let Some(slot) = entries.get_mut(position) {
                *slot = (track, value);
            }
        }
        Err(position) => entries.insert(position, (track, value)),
    }
}

fn coefficient_for(entries: &[(u32, [[f64; SUBSPACE_RANK]; 2])], track: u32) -> Option<&[[f64; SUBSPACE_RANK]; 2]> {
    let position = entries.binary_search_by_key(&track, |entry| entry.0).ok()?;
    entries.get(position).map(|entry| &entry.1)
}

fn clear_basis(basis: &mut [Option<[f64; SUBSPACE_RANK]>], range: Range<usize>) {
    let start = range.start.min(basis.len());
    let end = range.end.min(basis.len());
    if start < end {
        for slot in &mut basis[start..end] {
            *slot = None;
        }
    }
}

fn finish_run(run: RunWork, requested_end: usize, runs: &mut Vec<RunData>) {
    let end = requested_end.min(run.end);
    if run.start < end {
        runs.push(RunData { range: run.start..end, coefficients: run.coefficients });
    }
}

fn project_unassigned(data: &TrajectorySet, basis: &[Option<[f64; SUBSPACE_RANK]>], runs: &mut [RunData]) -> Result<(), SubspaceError> {
    for run in runs {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let mut candidate_ids = Vec::new();
        for frame in run.range.clone() {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            candidate_ids.extend_from_slice(data.index.at(frame));
        }
        candidate_ids.sort_unstable();
        candidate_ids.dedup();
        for id in candidate_ids {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(index) = usize::try_from(id).ok() else { continue };
            let Some(track) = data.tracks.get(index) else { continue };
            if track.rejected
                || track.points.len() < MIN_TRACK_LENGTH
                || complete_track_crosses_cut(data, track)
                || coefficient_for(&run.coefficients, id).is_some()
            {
                continue;
            }
            let Some(end) = track.start.checked_add(track.points.len()) else { return Err(SubspaceError::InvalidInput) };
            let lo = run.range.start.max(track.start);
            let hi = run.range.end.min(end).min(data.frames);
            let mut normal = [[0.0f64; SUBSPACE_RANK]; SUBSPACE_RANK];
            let mut rhs = [[0.0f64; SUBSPACE_RANK]; 2];
            let mut samples = 0usize;
            for frame in lo..hi {
                if cancelled() {
                    return Err(SubspaceError::Cancelled);
                }
                let Some(eigen) = basis.get(frame).and_then(|value| *value) else { continue };
                let Some(point) = track.points.get(frame.checked_sub(track.start).ok_or(SubspaceError::InvalidInput)?) else {
                    return Err(SubspaceError::InvalidInput);
                };
                samples = samples.saturating_add(1);
                for i in 0..SUBSPACE_RANK {
                    for j in 0..SUBSPACE_RANK {
                        normal[i][j] += eigen[i] * eigen[j];
                    }
                }
                for dimension in 0..2 {
                    for component in 0..SUBSPACE_RANK {
                        rhs[dimension][component] += eigen[component] * point[dimension] as f64;
                    }
                }
            }
            if samples == 0 {
                continue;
            }
            let mut rows = [[0.0f64; SUBSPACE_RANK]; 2];
            for dimension in 0..2 {
                rows[dimension] = solve_regularized(normal, rhs[dimension]).ok_or(SubspaceError::NumericalFailure)?;
            }
            set_coefficient(&mut run.coefficients, id, rows);
        }
    }
    Ok(())
}

fn reject_factorization_outliers(data: &TrajectorySet, basis: &[Option<[f64; SUBSPACE_RANK]>], runs: &mut [RunData]) -> Result<(), SubspaceError> {
    for run in runs {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let mut kept = Vec::with_capacity(run.coefficients.len());
        for (id, coefficients) in run.coefficients.drain(..) {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(index) = usize::try_from(id).ok() else { continue };
            let Some(track) = data.tracks.get(index) else { continue };
            if track.rejected || complete_track_crosses_cut(data, track) {
                continue;
            }
            let Some(end) = track.start.checked_add(track.points.len()) else { return Err(SubspaceError::InvalidInput) };
            let lo = run.range.start.max(track.start);
            let hi = run.range.end.min(end).min(data.frames);
            let mut observed = false;
            let mut bad = false;
            for frame in lo..hi {
                if cancelled() {
                    return Err(SubspaceError::Cancelled);
                }
                let Some(eigen) = basis.get(frame).and_then(|value| *value) else { continue };
                let Some(point) = track.points.get(frame.checked_sub(track.start).ok_or(SubspaceError::InvalidInput)?) else {
                    return Err(SubspaceError::InvalidInput);
                };
                let error = (dot(&coefficients[0], &eigen) - point[0] as f64).hypot(dot(&coefficients[1], &eigen) - point[1] as f64);
                if !error.is_finite() {
                    return Err(SubspaceError::NumericalFailure);
                }
                observed = true;
                if error > FACTOR_RESIDUAL_LIMIT {
                    bad = true;
                    break;
                }
            }
            if observed && !bad {
                kept.push((id, coefficients));
            }
        }
        run.coefficients = kept;
    }
    Ok(())
}

fn no_motion_anchor(
    data: &TrajectorySet,
    model: &SubspaceModel,
    previous_segment: &Range<usize>,
    previous_basis: [f64; SUBSPACE_RANK],
    segment: &Range<usize>,
) -> Option<NoMotionAnchor> {
    if previous_segment.end == 0 || segment.start >= data.frames || previous_segment.end > segment.start {
        return None;
    }
    // A source discontinuity is an explicit hard boundary even when the runs happen to be
    // adjacent in the retained basis.
    let first_cut = match data.segment_starts.binary_search(&previous_segment.end) {
        Ok(position) | Err(position) => position,
    };
    if data.segment_starts.get(first_cut).is_some_and(|cut| *cut <= segment.start) {
        return None;
    }
    let previous_frame = previous_segment.end.saturating_sub(1);
    let previous_ids = model.active_tracks(previous_frame);
    let current_ids = model.active_tracks(segment.start);
    let mut shared_tracks = Vec::new();
    for &id in previous_ids {
        if cancelled() || current_ids.binary_search(&id).is_err() {
            continue;
        }
        let Some(index) = usize::try_from(id).ok() else { continue };
        let Some(track) = data.tracks.get(index) else { continue };
        if track.rejected || model.coefficient(previous_frame, index).is_none() || model.coefficient(segment.start, index).is_none() {
            continue;
        }
        shared_tracks.push(id);
    }
    if shared_tracks.len() < MIN_COMPLETE_TRACKS {
        return None;
    }
    Some(NoMotionAnchor { old_frame: previous_frame, previous_basis, shared_tracks })
}

fn fit_constant_basis(
    data: &TrajectorySet,
    model: &SubspaceModel,
    segment: &Range<usize>,
    anchor: Option<&NoMotionAnchor>,
) -> Result<Option<[f64; SUBSPACE_RANK]>, SubspaceError> {
    if segment.start >= segment.end || segment.end > data.frames {
        return Err(SubspaceError::InvalidInput);
    }
    let mut normal = [[0.0f64; SUBSPACE_RANK]; SUBSPACE_RANK];
    let mut rhs = [0.0f64; SUBSPACE_RANK];
    let mut samples = 0usize;
    for frame in segment.clone() {
        if cancelled() {
            return Err(SubspaceError::Cancelled);
        }
        let Some(eigen) = model.basis.get(frame).and_then(|value| *value) else { continue };
        for &id in model.active_tracks(frame) {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(index) = usize::try_from(id).ok() else { continue };
            let Some(track) = data.tracks.get(index) else { continue };
            if track.rejected {
                continue;
            }
            let Some(coefficients) = model.coefficient(frame, index) else { continue };
            let offset = match frame.checked_sub(track.start) {
                Some(value) => value,
                None => continue,
            };
            let Some(_point) = track.points.get(offset) else { continue };
            for dimension in 0..2 {
                let row = coefficients[dimension];
                let target = dot(&row, &eigen);
                for i in 0..SUBSPACE_RANK {
                    for j in 0..SUBSPACE_RANK {
                        normal[i][j] += row[i] * row[j];
                    }
                    rhs[i] += row[i] * target;
                }
            }
            samples = samples.saturating_add(1);
        }
    }

    let mut anchor_samples = 0usize;
    if let Some(anchor) = anchor {
        let span = segment.end.saturating_sub(segment.start).max(1) as f64;
        // Per (track, dimension) anchor row: the segment data contributes one row per frame for
        // the same track-dimension, so this keeps the anchor at >=1e2 relative weight.
        let weight = span * NO_MOTION_ANCHOR_MULTIPLIER;
        if !weight.is_finite() || weight <= 0.0 {
            return Err(SubspaceError::NumericalFailure);
        }
        for &id in &anchor.shared_tracks {
            if cancelled() {
                return Err(SubspaceError::Cancelled);
            }
            let Some(index) = usize::try_from(id).ok() else { continue };
            let Some(old_coefficients) = model.coefficient(anchor.old_frame, index) else { continue };
            let Some(new_coefficients) = model.coefficient(segment.start, index) else { continue };
            for dimension in 0..2 {
                let old_target = dot(&old_coefficients[dimension], &anchor.previous_basis);
                if !old_target.is_finite() {
                    return Err(SubspaceError::NumericalFailure);
                }
                let row = new_coefficients[dimension];
                for i in 0..SUBSPACE_RANK {
                    for j in 0..SUBSPACE_RANK {
                        normal[i][j] += weight * row[i] * row[j];
                    }
                    rhs[i] += weight * row[i] * old_target;
                }
                anchor_samples = anchor_samples.saturating_add(1);
            }
        }
    }
    if samples == 0 && anchor_samples == 0 {
        return Ok(None);
    }
    let value = solve_regularized(normal, rhs).ok_or(SubspaceError::NumericalFailure)?;
    Ok(Some(value))
}

fn dot(a: &[f64; SUBSPACE_RANK], b: &[f64; SUBSPACE_RANK]) -> f64 {
    let mut sum = 0.0;
    for i in 0..SUBSPACE_RANK {
        sum += a[i] * b[i];
    }
    sum
}

#[inline]
fn cancelled() -> bool {
    filmcraft_media::cancel::cancelled()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{RunData, SUBSPACE_RANK, SubspaceModel, TRANSITION_RESIDUAL_LIMIT, factorize, planned_basis};
    use crate::vfx::temporal::trajectory_data::{FeatureTrajectory, TrajectorySet};
    use crate::vfx::temporal::trajectory_index::TrajectoryIndex;

    fn make_tracks(_frames: usize, count: usize, start: usize, length: usize, moving_after: Option<usize>) -> Vec<FeatureTrajectory> {
        (0..count)
            .map(|index| {
                let points = (0..length)
                    .map(|offset| {
                        let frame = start + offset;
                        let t = frame as f64;
                        let i = index as f64;
                        let mut x = 15.0 + i * i * 0.07 + t * 0.18 + i * t * 0.001;
                        let mut y = 26.0 + (i * 0.71).sin() * 11.0 - t * 0.12 + i * t * 0.0007;
                        let noise = (((frame.saturating_mul(31).saturating_add(index.saturating_mul(17))) % 13) as f64 - 6.0) * 0.01;
                        x += noise;
                        y -= noise * 0.5;
                        if let Some(cut) = moving_after
                            && frame >= cut
                        {
                            let dt = (frame - cut) as f64;
                            x += dt * 1.7;
                            y -= dt * 0.9;
                        }
                        [x as f32, y as f32]
                    })
                    .collect();
                FeatureTrajectory { start, points, rejected: false }
            })
            .collect()
    }
    /// Add a deterministic, newly observable motion component after `moving_after`.  The
    /// frequency depends on the track id, so the suffix genuinely grows the shared temporal rank
    /// instead of merely changing one global translation.  The component starts at zero, keeping
    /// the source motion continuous at the rank-growth boundary.
    fn make_tracks_with(count: usize, start: usize, length: usize, moving_after: Option<usize>, scale: f64) -> Vec<FeatureTrajectory> {
        let mut tracks = make_tracks(length, count, start, length, moving_after);
        for (index, track) in tracks.iter_mut().enumerate() {
            for (offset, point) in track.points.iter_mut().enumerate() {
                let frame = start + offset;
                let Some(cut) = moving_after else { continue };
                if frame < cut {
                    continue;
                }
                let dt = (frame - cut) as f64;
                let frequency = 0.11 + index as f64 * 0.017;
                let amplitude = scale * (index.saturating_add(1) as f64) * dt;
                let phase = dt * frequency;
                point[0] += (amplitude * phase.sin()) as f32;
                point[1] += (amplitude * phase.cos()) as f32;
            }
        }
        tracks
    }

    fn set(frames: usize, tracks: Vec<FeatureTrajectory>) -> TrajectorySet {
        TrajectorySet::new(640, 360, frames, tracks, vec![0], Vec::new()).expect("checked synthetic trajectory set")
    }

    #[test]
    fn low_rank_noisy_tracks_keep_local_coefficients_and_reconstruct() {
        let data = set(64, make_tracks(64, 24, 0, 64, None));
        let model = factorize(&data, &mut |_| {}).expect("low-rank tracks factorise");
        assert!(!model.segments.is_empty());
        let basis = model.basis[30].expect("middle frame has a basis");
        let coefficients = model.coefficient(30, 4).expect("frame-local coefficient exists");
        let x = coefficients[0].iter().zip(basis).map(|(a, b)| a * b).sum::<f64>();
        let y = coefficients[1].iter().zip(basis).map(|(a, b)| a * b).sum::<f64>();
        let point = data.tracks[4].points[30];
        assert!((x - point[0] as f64).hypot(y - point[1] as f64) < 1.0);
        let planned = planned_basis(&data, &model, 0.0, false).expect("zero smoothing preserves basis");
        assert!(planned[30].is_some());
        assert!(planned[63].is_some());
    }

    #[test]
    fn unsupported_track_gap_resumes_in_a_new_run() {
        let first = make_tracks(100, 12, 0, 35, None);
        let later = make_tracks(100, 12, 50, 50, Some(75));
        let mut tracks = first;
        tracks.extend(later);
        let data = set(100, tracks);
        let model = factorize(&data, &mut |_| {}).expect("later supported run is retained");
        assert!(model.segments.len() >= 2);
        assert!(model.coefficient(80, 12).is_some());
        assert!(model.basis[80].is_some());
    }

    #[test]
    fn newly_observed_motion_reinitialises_after_static_prefix() {
        let data = set(100, make_tracks(100, 24, 0, 100, Some(50)));
        let model = factorize(&data, &mut |_| {}).expect("moving suffix factorises");
        assert!(model.segments.len() >= 2);
        assert!(model.coefficient(90, 3).is_some());
        assert!(model.basis[90].is_some());
    }

    #[test]
    fn malformed_mutation_is_rejected_before_numeric_work() {
        let mut data = set(32, make_tracks(32, 8, 0, 32, None));
        data.tracks[0].points[2][0] = f32::NAN;
        assert!(factorize(&data, &mut |_| {}).is_err());
        assert!(TrajectorySet::new(640, 360, 32, make_tracks(32, 1, 31, 2, None), vec![0], Vec::new()).is_none());
    }

    #[test]
    fn no_motion_is_constant_per_supported_run() {
        let data = set(64, make_tracks(64, 24, 0, 64, None));
        let model = factorize(&data, &mut |_| {}).expect("tracks factorise");
        let planned = planned_basis(&data, &model, 12.0, true).expect("constant basis fit");
        for segment in &model.segments {
            let Some(first) = planned[segment.start] else { continue };
            for frame in segment.clone() {
                let Some(value) = planned[frame] else { continue };
                assert_eq!(value, first);
                assert!(value.iter().all(|component| component.is_finite()));
            }
        }
    }

    #[test]
    fn independent_dynamic_track_with_large_residual_is_excluded() {
        let mut tracks = make_tracks(80, 24, 0, 80, None);
        let outlier_points = (0..40)
            .map(|offset| {
                let frame = 30 + offset;
                [((frame * 17 % 53) as f32) * 4.0, ((frame * 29 % 47) as f32) * 3.0]
            })
            .collect();
        tracks.push(FeatureTrajectory { start: 30, points: outlier_points, rejected: false });
        let data = set(80, tracks);
        let model = factorize(&data, &mut |_| {}).expect("normal tracks factorise");
        assert!(model.coefficient(60, 24).is_none());
    }
    #[test]
    fn cancellation_stops_factorisation_before_projection() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let data = set(64, make_tracks(64, 24, 0, 64, None));
        let flag = Arc::new(AtomicBool::new(true));
        let result = filmcraft_media::cancel::with_cancel(&flag, || super::factorize(&data, &mut |_| {}));
        assert!(matches!(result, Err(super::SubspaceError::Cancelled)));
        flag.store(false, Ordering::Relaxed);
    }

    #[test]
    fn window_shrinks_until_redundant_track_target_is_met() {
        let mut tracks = make_tracks(50, 5, 0, 50, None);
        tracks.extend(make_tracks(50, 20, 0, 30, None));
        let data = set(50, tracks);
        let (length, complete) = super::choose_window(&data, &(0..50), 0).expect("feasible synthetic window");
        assert_eq!(length, 30);
        assert!(complete.len() >= super::TARGET_COMPLETE_TRACKS);
    }

    #[test]
    fn no_motion_recovery_anchors_shared_scene_points() {
        let data = set(100, make_tracks(100, 24, 0, 100, Some(50)));
        let model = factorize(&data, &mut |_| {}).expect("moving suffix factorises");
        assert!(model.segments.len() >= 2);
        let boundary = model.segments[1].start;
        let previous_frame = boundary.saturating_sub(1);
        let planned = planned_basis(&data, &model, 0.0, true).expect("NoMotion basis");
        let old_basis = planned[previous_frame].expect("previous constant");
        let new_basis = planned[boundary].expect("new constant");
        let old_coefficients = model.coefficient(previous_frame, 3).expect("shared old coefficients");
        let new_coefficients = model.coefficient(boundary, 3).expect("shared new coefficients");
        let old_point = [
            old_coefficients[0].iter().zip(old_basis).map(|(a, b)| a * b).sum::<f64>(),
            old_coefficients[1].iter().zip(old_basis).map(|(a, b)| a * b).sum::<f64>(),
        ];
        let new_point = [
            new_coefficients[0].iter().zip(new_basis).map(|(a, b)| a * b).sum::<f64>(),
            new_coefficients[1].iter().zip(new_basis).map(|(a, b)| a * b).sum::<f64>(),
        ];
        assert!(old_point.iter().chain(new_point.iter()).all(|value| value.is_finite()));
        assert!((old_point[0] - new_point[0]).hypot(old_point[1] - new_point[1]) < 3.0);
    }

    #[test]
    fn smooth_motion_keeps_planned_features_continuous_across_restart() {
        let data = set(120, make_tracks_with(24, 0, 120, Some(50), 0.05));
        let model = factorize(&data, &mut |_| {}).expect("rank-growth restart factorises");
        assert!(model.segments.len() >= 2);
        let raw = planned_basis(&data, &model, 0.0, false).expect("raw basis");
        assert_eq!(raw, model.basis);
        let boundary = model.segments[1].start;
        let planned = planned_basis(&data, &model, 12.0, false).expect("Smooth Motion basis");
        let step = |track: usize, frame: usize| {
            let coefficients = model.coefficient(frame, track).expect("shared track coefficients");
            let basis = planned[frame].expect("planned basis at boundary");
            [coefficients[0].iter().zip(basis).map(|(a, b)| a * b).sum::<f64>(), coefficients[1].iter().zip(basis).map(|(a, b)| a * b).sum::<f64>()]
        };
        let observed = |track: usize, frame: usize| {
            let point = data.tracks[track].points[frame];
            [f64::from(point[0]), f64::from(point[1])]
        };
        for track in 0..4 {
            let previous = step(track, boundary - 1);
            let first = step(track, boundary);
            let raw_previous = observed(track, boundary - 1);
            let raw_first = observed(track, boundary);
            // The shared scene feature itself moves only a couple of pixels per frame in this
            // fixture, so the smoothed plan must not add a large artificial step exactly at the
            // supported restart.
            let before = (first[0] - previous[0]).hypot(first[1] - previous[1]);
            let actual_motion = (raw_first[0] - raw_previous[0]).hypot(raw_first[1] - raw_previous[1]);
            assert!(before < 1.0 + actual_motion, "restart step for track {track}: planned {before:.3}px, observed {actual_motion:.3}px");
        }
    }

    #[test]
    fn rank_deficient_bridge_preserves_new_run_component_for_active_track() {
        const FRAMES: usize = 40;
        const BOUNDARY: usize = 16;
        const BRIDGE_END: usize = BOUNDARY + 10;

        let mut tracks = (0..5)
            .map(|_| {
                let points = (0..FRAMES).map(|frame| if frame < BOUNDARY { [1.0, 2.0] } else { [3.0, 4.0] }).collect();
                FeatureTrajectory { start: 0, points, rejected: false }
            })
            .collect::<Vec<_>>();
        tracks.push(FeatureTrajectory { start: BOUNDARY, points: vec![[7.0, 0.0]; FRAMES - BOUNDARY], rejected: false });
        let data = TrajectorySet::new(640, 360, FRAMES, tracks, vec![0], Vec::new()).expect("rank-deficient fixture");

        let mut old_basis = [0.0; SUBSPACE_RANK];
        old_basis[0] = 1.0;
        old_basis[1] = 2.0;
        old_basis[8] = 0.0;
        let mut new_basis = [0.0; SUBSPACE_RANK];
        new_basis[0] = 3.0;
        new_basis[1] = 4.0;
        new_basis[8] = 7.0;
        let basis = (0..FRAMES).map(|frame| Some(if frame < BOUNDARY { old_basis } else { new_basis })).collect();

        let shared_coefficients = (0..5)
            .map(|id| {
                let mut coefficients = [[0.0; SUBSPACE_RANK]; 2];
                coefficients[0][0] = 1.0;
                coefficients[1][1] = 1.0;
                (u32::try_from(id).expect("small fixture id"), coefficients)
            })
            .collect::<Vec<_>>();
        let mut new_coefficients = shared_coefficients.clone();
        let mut independent = [[0.0; SUBSPACE_RANK]; 2];
        independent[0][8] = 1.0;
        new_coefficients.push((5, independent));

        let index = TrajectoryIndex::new(&data.tracks, FRAMES).expect("fixture index");
        let model = SubspaceModel {
            basis,
            segments: vec![0..BOUNDARY, BOUNDARY..FRAMES],
            runs: vec![RunData { range: 0..BOUNDARY, coefficients: shared_coefficients }, RunData { range: BOUNDARY..FRAMES, coefficients: new_coefficients }],
            index: Arc::new(index),
        };
        let planned = planned_basis(&data, &model, 3.0, false).expect("rank-deficient bridge");
        const { assert!(BRIDGE_END + 1 < FRAMES, "bridge must complete inside the new run") };

        let target = |frame: usize, track: usize| {
            let coefficients = model.coefficient(frame, track).expect("active track");
            let value = planned[frame].expect("planned bridge basis");
            [coefficients[0].iter().zip(value).map(|(a, b)| a * b).sum::<f64>(), coefficients[1].iter().zip(value).map(|(a, b)| a * b).sum::<f64>()]
        };
        let native_target = |frame: usize, track: usize| {
            let coefficients = model.coefficient(frame, track).expect("native track");
            let value = model.basis[frame].expect("native basis");
            [coefficients[0].iter().zip(value).map(|(a, b)| a * b).sum::<f64>(), coefficients[1].iter().zip(value).map(|(a, b)| a * b).sum::<f64>()]
        };

        for frame in BOUNDARY..=BRIDGE_END {
            let value = target(frame, 5);
            assert!((value[0] - 7.0).abs() < 1.0e-8, "native component lost at bridge frame {frame}: {value:?}");
        }
        for track in 0..=5 {
            let endpoint = target(BRIDGE_END, track);
            let native_endpoint = native_target(BRIDGE_END, track);
            assert!(
                (endpoint[0] - native_endpoint[0]).hypot(endpoint[1] - native_endpoint[1]) < 1.0e-8,
                "track {track} endpoint is not native: {endpoint:?} != {native_endpoint:?}"
            );
        }
        let endpoint = target(BRIDGE_END, 5);
        let next = target(BRIDGE_END + 1, 5);
        assert!((endpoint[0] - next[0]).hypot(endpoint[1] - next[1]) < 1.0e-8, "jump after bridge completion: {endpoint:?} -> {next:?}");
    }

    #[test]
    fn expiring_boundary_anchor_keeps_nonshared_component_smooth() {
        const FRAMES: usize = 40;
        const BOUNDARY: usize = 16;
        const BRIDGE_LEN: usize = 10;
        const SIXTH_END: usize = BOUNDARY + 5;
        const BRIDGE_END: usize = BOUNDARY + BRIDGE_LEN;

        let mut tracks = (0..5).map(|_| FeatureTrajectory { start: 0, points: vec![[0.0, 0.0]; FRAMES], rejected: false }).collect::<Vec<_>>();
        // This sixth boundary-shared row is the only shared constraint on component eight, then
        // expires halfway through the staged bridge.
        tracks.push(FeatureTrajectory { start: 0, points: vec![[0.0, 0.0]; SIXTH_END], rejected: false });
        // The active non-shared consumer keeps observing that component after the anchor expires.
        tracks.push(FeatureTrajectory { start: BOUNDARY, points: vec![[0.0, 0.0]; FRAMES - BOUNDARY], rejected: false });
        let data = TrajectorySet::new(640, 360, FRAMES, tracks, vec![0], Vec::new()).expect("expiring-anchor fixture");

        let mut old_basis = [0.0; SUBSPACE_RANK];
        old_basis[0] = 1.0;
        old_basis[1] = 2.0;
        let mut new_basis = [0.0; SUBSPACE_RANK];
        new_basis[0] = 3.0;
        new_basis[1] = 4.0;
        new_basis[8] = 7.0;
        let basis = (0..FRAMES).map(|frame| Some(if frame < BOUNDARY { old_basis } else { new_basis })).collect();

        let shared_coefficients = (0..6)
            .map(|id| {
                let mut coefficients = [[0.0; SUBSPACE_RANK]; 2];
                coefficients[0][if id == 5 { 8 } else { 0 }] = 1.0;
                coefficients[1][1] = 1.0;
                (u32::try_from(id).expect("small fixture id"), coefficients)
            })
            .collect::<Vec<_>>();
        let mut new_coefficients = shared_coefficients.clone();
        let mut independent = [[0.0; SUBSPACE_RANK]; 2];
        independent[0][8] = 1.0;
        new_coefficients.push((6, independent));

        let index = TrajectoryIndex::new(&data.tracks, FRAMES).expect("fixture index");
        let model = SubspaceModel {
            basis,
            segments: vec![0..BOUNDARY, BOUNDARY..FRAMES],
            runs: vec![RunData { range: 0..BOUNDARY, coefficients: shared_coefficients }, RunData { range: BOUNDARY..FRAMES, coefficients: new_coefficients }],
            index: Arc::new(index),
        };
        let planned = planned_basis(&data, &model, 3.0, false).expect("expiring-anchor bridge");
        assert!(!model.active_tracks(SIXTH_END).contains(&5), "sixth anchor must expire before the bridge ends");

        let target = |frame: usize, track: usize| {
            let coefficients = model.coefficient(frame, track).expect("fixed run coefficient");
            let value = planned[frame].expect("planned bridge basis");
            [coefficients[0].iter().zip(value).map(|(a, b)| a * b).sum::<f64>(), coefficients[1].iter().zip(value).map(|(a, b)| a * b).sum::<f64>()]
        };
        let native_target = |frame: usize, track: usize| {
            let coefficients = model.coefficient(frame, track).expect("native coefficient");
            let value = model.basis[frame].expect("native basis");
            [coefficients[0].iter().zip(value).map(|(a, b)| a * b).sum::<f64>(), coefficients[1].iter().zip(value).map(|(a, b)| a * b).sum::<f64>()]
        };

        for frame in BOUNDARY..=BRIDGE_END {
            let u = (frame - BOUNDARY) as f64 / BRIDGE_LEN as f64;
            let alpha = u * u * (3.0 - 2.0 * u);
            let value = target(frame, 6);
            assert!((value[0] - 7.0 * alpha).abs() < 1.0e-6, "non-shared component snapped at frame {frame}: {value:?}, expected {:.6}", 7.0 * alpha);
        }
        for track in 0..=6 {
            let endpoint = target(BRIDGE_END, track);
            let native_endpoint = native_target(BRIDGE_END, track);
            assert!(
                (endpoint[0] - native_endpoint[0]).hypot(endpoint[1] - native_endpoint[1]) < 1.0e-8,
                "track {track} endpoint is not native: {endpoint:?} != {native_endpoint:?}"
            );
        }
        let endpoint = target(BRIDGE_END, 6);
        let next = target(BRIDGE_END + 1, 6);
        assert!((endpoint[0] - next[0]).hypot(endpoint[1] - next[1]) < 1.0e-8, "jump after bridge completion: {endpoint:?} -> {next:?}");
    }

    #[test]
    fn incompatible_restart_tapers_edge_to_native_boundary() {
        const FRAMES: usize = 30;
        const BOUNDARY: usize = 15;

        let tracks = (0..5).map(|_| FeatureTrajectory { start: 0, points: vec![[0.0, 0.0]; FRAMES], rejected: false }).collect::<Vec<_>>();
        let data = TrajectorySet::new(640, 360, FRAMES, tracks, vec![0], Vec::new()).expect("incompatible-row fixture");

        // The old edge-normalised plan contains a component that the new run cannot observe,
        // while both native boundary factors reconstruct the same zero-valued source point.
        let mut old_basis = [0.0; SUBSPACE_RANK];
        old_basis[8] = 6.0;
        let mut basis = Vec::with_capacity(FRAMES);
        for frame in 0..FRAMES {
            let value = if frame + 1 == BOUNDARY || frame >= BOUNDARY { [0.0; SUBSPACE_RANK] } else { old_basis };
            basis.push(Some(value));
        }

        let old_coefficients = (0..5)
            .map(|id| {
                let mut coefficients = [[0.0; SUBSPACE_RANK]; 2];
                coefficients[0][8] = 1.0;
                (u32::try_from(id).expect("small fixture id"), coefficients)
            })
            .collect::<Vec<_>>();
        let new_coefficients = (0..5)
            .map(|id| {
                let mut coefficients = [[0.0; SUBSPACE_RANK]; 2];
                coefficients[0][0] = 1.0;
                (u32::try_from(id).expect("small fixture id"), coefficients)
            })
            .collect::<Vec<_>>();
        let index = TrajectoryIndex::new(&data.tracks, FRAMES).expect("fixture index");
        let model = SubspaceModel {
            basis,
            segments: vec![0..BOUNDARY, BOUNDARY..FRAMES],
            runs: vec![RunData { range: 0..BOUNDARY, coefficients: old_coefficients }, RunData { range: BOUNDARY..FRAMES, coefficients: new_coefficients }],
            index: Arc::new(index),
        };

        let planned = planned_basis(&data, &model, 3.0, false).expect("edge fallback");
        let target = |frame: usize| {
            let coefficients = model.coefficient(frame, 0).expect("shared fixture row");
            let basis = planned[frame].expect("planned edge basis");
            coefficients[0].iter().zip(basis).map(|(a, b)| a * b).sum::<f64>()
        };
        let boundary_step = (target(BOUNDARY) - target(BOUNDARY - 1)).abs();
        assert!(boundary_step < 1.0e-8, "native boundary was not continuous: {boundary_step}");

        let maximum_step = (1..FRAMES).map(|frame| (target(frame) - target(frame - 1)).abs()).fold(0.0, f64::max);
        assert!(maximum_step < TRANSITION_RESIDUAL_LIMIT, "edge taper introduced a visible step: {maximum_step}");
    }

    #[test]
    fn source_cut_boundary_is_not_bridged() {
        // Two separated shots: declared segment starts, no shared observations across the cut.
        let first = make_tracks(100, 12, 0, 45, None);
        let mut second = make_tracks_with(12, 45, 55, Some(75), 0.03);
        for track in &mut second {
            for point in &mut track.points {
                point[0] += 180.0;
                point[1] -= 70.0;
            }
        }
        let mut tracks = first;
        tracks.extend(second);
        let data = TrajectorySet::new(640, 360, 100, tracks, vec![0, 45], Vec::new()).expect("cut trajectory set");
        let model = factorize(&data, &mut |_| {}).expect("both shots factorise");
        let planned = planned_basis(&data, &model, 12.0, false).expect("Smooth Motion basis");
        for frame in [43, 44, 45, 46] {
            assert!(planned[frame].is_some(), "frame {frame} stays supported across the cut");
        }

        let second_segment = model.segments.iter().find(|segment| segment.start == 45).expect("cut starts a retained run");
        let frame = second_segment.start;
        let radius = (12.0_f64 * (10.0 / 3.0)).ceil() as usize;
        let sigma = (radius as f64 / 2.0_f64.sqrt()).max(1.0e-9);
        let lo = frame.saturating_sub(radius).max(second_segment.start);
        let hi = frame.saturating_add(radius).min(second_segment.end.saturating_sub(1));
        let mut expected = [0.0; super::SUBSPACE_RANK];
        let mut weight_sum = 0.0;
        for neighbour in lo..=hi {
            let value = model.basis[neighbour].expect("second-shot basis");
            let distance = neighbour as isize - frame as isize;
            let d = distance as f64;
            let weight = (-0.5 * (d / sigma) * (d / sigma)).exp();
            weight_sum += weight;
            for component in 0..super::SUBSPACE_RANK {
                expected[component] += weight * value[component];
            }
        }
        for value in &mut expected {
            *value /= weight_sum;
        }
        let actual = planned[frame].expect("second-shot planned basis");
        assert!(actual.iter().zip(expected).all(|(actual, expected)| (*actual - expected).abs() < 1.0e-9));

        // The consumer targets remain distinct across a true cut, proving that the transition
        // fitter did not invent a cross-shot blend just because the frame ranges are adjacent.
        let target = |frame: usize, track: usize| {
            let coefficients = model.coefficient(frame, track).expect("cut track coefficients");
            let basis = planned[frame].expect("cut planned basis");
            [coefficients[0].iter().zip(basis).map(|(a, b)| a * b).sum::<f64>(), coefficients[1].iter().zip(basis).map(|(a, b)| a * b).sum::<f64>()]
        };
        let old_target = target(44, 0);
        let new_target = target(45, 12);
        assert!((old_target[0] - new_target[0]).hypot(old_target[1] - new_target[1]) > 50.0);
    }
}
