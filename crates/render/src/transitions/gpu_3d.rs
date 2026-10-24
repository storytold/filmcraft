//! Typed planning for the legacy and projective 3D transition family.
//!
//! Flip Motion, 3D Spin, Spinback, and Slice have wired kernels; the other routes remain fail-closed pending parity kernels.

use filmcraft_project::{EffectInstance, EffectKind, find_effect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ThreeDKind {
    FlipMotion = 87,
    CubeSpin = 88,
    FlipOver = 89,
    FoldMotion = 90,
    Slice = 91,
    Spin3d = 92,
    Spinback3d = 93,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThreeDPayload {
    LegacyAffine,
    FlipMotion { direction: u32, perspective_percent: f32, motion_blur_percent: f32 },
    CardScene { direction: u32, perspective_percent: f32 },
    Fold { direction: u32, perspective_percent: f32 },
    Slice { slices: f32, angle_degrees: f32, motion_blur_percent: f32 },
    Spinback { direction: u32, depth_percent: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreeDOp {
    pub kind: ThreeDKind,
    pub payload: ThreeDPayload,
    pub progress: f32,
}

pub fn matches_id(id: &str) -> bool {
    matches!(id, "cube_spin" | "flip_motion" | "flip_over" | "fold_motion" | "slice" | "spin_3d" | "spinback_3d")
}

/// Catalog IDs only; the catalog dispatcher separately validates supported opcodes.
pub fn eval(e: &EffectInstance, progress: f32) -> Option<ThreeDOp> {
    let id = e.effect.as_str();
    let kind = match id {
        "cube_spin" => ThreeDKind::CubeSpin,
        "flip_motion" => ThreeDKind::FlipMotion,
        "flip_over" => ThreeDKind::FlipOver,
        "fold_motion" => ThreeDKind::FoldMotion,
        "slice" => ThreeDKind::Slice,
        "spin_3d" => ThreeDKind::Spin3d,
        "spinback_3d" => ThreeDKind::Spinback3d,
        _ => return None,
    };
    if find_effect(id).is_none_or(|d| d.kind != EffectKind::VideoTransition) {
        return None;
    }
    let direction = || {
        e.param("direction")
            .and_then(|p| match p.value {
                filmcraft_project::ParamValue::Choice(v) => Some(v),
                _ => None,
            })
            .unwrap_or(3)
    };
    let number = |name: &str| e.f64_at(name, filmcraft_time::Tick::ZERO) as f32;
    let payload = match kind {
        ThreeDKind::CubeSpin | ThreeDKind::FlipOver => ThreeDPayload::LegacyAffine,
        ThreeDKind::FlipMotion => {
            ThreeDPayload::FlipMotion { direction: direction(), perspective_percent: number("perspective"), motion_blur_percent: number("motion_blur") }
        }
        ThreeDKind::FoldMotion => ThreeDPayload::Fold { direction: direction(), perspective_percent: number("perspective") },
        ThreeDKind::Slice => ThreeDPayload::Slice {
            slices: number("slices").round().clamp(1.0, 64.0),
            angle_degrees: number("angle"),
            motion_blur_percent: number("motion_blur"),
        },
        ThreeDKind::Spin3d => ThreeDPayload::CardScene { direction: direction(), perspective_percent: number("perspective") },
        ThreeDKind::Spinback3d => ThreeDPayload::Spinback { direction: direction(), depth_percent: number("depth") },
    };
    Some(ThreeDOp { kind, payload, progress })
}

pub const fn gpu_ok(op: &ThreeDOp) -> bool {
    match op.payload {
        ThreeDPayload::FlipMotion { direction, perspective_percent, motion_blur_percent } => {
            matches!(op.kind, ThreeDKind::FlipMotion)
                && direction <= 3
                && perspective_percent.is_finite()
                && perspective_percent >= 0.0
                && perspective_percent <= 100.0
                && motion_blur_percent.is_finite()
                && motion_blur_percent >= 0.0
                && motion_blur_percent <= 100.0
                && op.progress.is_finite()
                && op.progress >= 0.0
                && op.progress <= 1.0
        }
        ThreeDPayload::CardScene { direction, perspective_percent } => {
            matches!(op.kind, ThreeDKind::Spin3d)
                && direction <= 3
                && perspective_percent.is_finite()
                && perspective_percent >= 0.0
                && perspective_percent <= 100.0
                && op.progress.is_finite()
                && op.progress >= 0.0
                && op.progress <= 1.0
        }
        ThreeDPayload::Slice { slices, angle_degrees, motion_blur_percent } => {
            matches!(op.kind, ThreeDKind::Slice)
                && slices.is_finite()
                && slices >= 1.0
                && slices <= 64.0
                && angle_degrees.is_finite()
                && motion_blur_percent.is_finite()
                && motion_blur_percent >= 0.0
                && motion_blur_percent <= 100.0
                && op.progress.is_finite()
                && op.progress >= 0.0
                && op.progress <= 1.0
        }
        ThreeDPayload::Spinback { direction, depth_percent } => {
            matches!(op.kind, ThreeDKind::Spinback3d)
                && direction <= 3
                && depth_percent.is_finite()
                && depth_percent >= 0.0
                && depth_percent <= 100.0
                && op.progress.is_finite()
                && op.progress >= 0.0
                && op.progress <= 1.0
        }
        _ => false,
    }
}

/// Catalog uniform words: progress, opcode, direction, then perspective and shutter percent.
pub fn pack(op: &ThreeDOp) -> Option<[u32; 32]> {
    if !gpu_ok(op) {
        return None;
    }
    let mut words = [0; 32];
    words[0] = op.progress.to_bits();
    match op.payload {
        ThreeDPayload::FlipMotion { direction, perspective_percent, motion_blur_percent } => {
            words[1] = ThreeDKind::FlipMotion as u32;
            words[2] = direction;
            words[12] = perspective_percent.to_bits();
            words[13] = motion_blur_percent.to_bits();
        }
        ThreeDPayload::CardScene { direction, perspective_percent } if op.kind == ThreeDKind::Spin3d => {
            words[1] = ThreeDKind::Spin3d as u32;
            words[2] = direction;
            words[12] = perspective_percent.to_bits();
        }
        ThreeDPayload::Slice { slices, angle_degrees, motion_blur_percent } if op.kind == ThreeDKind::Slice => {
            words[1] = ThreeDKind::Slice as u32;
            words[12] = slices.to_bits();
            words[13] = angle_degrees.to_bits();
            words[14] = motion_blur_percent.to_bits();
        }
        ThreeDPayload::Spinback { direction, depth_percent } if op.kind == ThreeDKind::Spinback3d => {
            words[1] = ThreeDKind::Spinback3d as u32;
            words[2] = direction;
            words[12] = depth_percent.to_bits();
        }
        _ => return None,
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_opcodes_are_stable_and_routes_fail_closed() {
        assert_eq!(
            [
                ThreeDKind::FlipMotion as u32,
                ThreeDKind::CubeSpin as u32,
                ThreeDKind::FlipOver as u32,
                ThreeDKind::FoldMotion as u32,
                ThreeDKind::Slice as u32,
                ThreeDKind::Spin3d as u32,
                ThreeDKind::Spinback3d as u32
            ],
            [87, 88, 89, 90, 91, 92, 93]
        );
        assert!(gpu_ok(&ThreeDOp {
            kind: ThreeDKind::FlipMotion,
            payload: ThreeDPayload::FlipMotion { direction: 3, perspective_percent: 50.0, motion_blur_percent: 50.0 },
            progress: 0.5
        }));
        assert!(gpu_ok(&ThreeDOp {
            kind: ThreeDKind::Slice,
            payload: ThreeDPayload::Slice { slices: 8.0, angle_degrees: 20.0, motion_blur_percent: 50.0 },
            progress: 0.5
        }));
        assert!(gpu_ok(&ThreeDOp { kind: ThreeDKind::Spinback3d, payload: ThreeDPayload::Spinback { direction: 3, depth_percent: 60.0 }, progress: 0.5 }));
        assert!(!gpu_ok(&ThreeDOp { kind: ThreeDKind::CubeSpin, payload: ThreeDPayload::LegacyAffine, progress: 0.5 }));
    }
}
