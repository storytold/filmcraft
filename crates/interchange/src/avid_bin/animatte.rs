//! Bounded read-only decoding of the default Key In AniMatte shape records and one-point motion.
//! Unknown settings, reference layouts, and payload revisions remain unsupported.
use super::{Bin, ByteOrder, bad};
use crate::Result;
use serde::{Deserialize, Serialize};

const DEFAULT_HEADER: [u8; 268] = [
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x04, 0x40, 0x59, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
    0x32, 0x00, 0x00, 0x00, 0x00, 0x40, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x60, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xeb, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xeb, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x64, 0x00, 0x01, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00,
    0x10, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0x00,
    0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0x7f, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x20, 0x3f, 0xf0,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const MAX_STATES: usize = 128;
const MAX_SHAPES: usize = 64;
const MAX_POINTS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AniMattePoint {
    pub xy: [f64; 2],
    pub incoming: [f64; 2],
    pub outgoing: [f64; 2],
    pub corner: bool,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AniMatteShape {
    pub id: u32,
    pub points: Vec<AniMattePoint>,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AniMatteState {
    pub frame: u32,
    pub shapes: Vec<AniMatteShape>,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AniMatteData {
    pub states: Vec<AniMatteState>,
    /// Sampled foreground Point A coordinates in normalized frame space.
    pub tracker_xy: Option<Vec<[f64; 2]>>,
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn le64f(data: &[u8], at: usize) -> Option<f64> {
    let v = f64::from_le_bytes(data.get(at..at.checked_add(8)?)?.try_into().ok()?);
    v.is_finite().then_some(v)
}
fn be64f(data: &[u8], at: usize) -> Option<f64> {
    let v = f64::from_be_bytes(data.get(at..at.checked_add(8)?)?.try_into().ok()?);
    (v.is_finite() && v.abs() <= 1_000_000.0).then_some(v)
}
fn default_header(data: &[u8]) -> bool {
    data.len() == DEFAULT_HEADER.len() && data.iter().enumerate().all(|(i, v)| (4..8).contains(&i) || *v == DEFAULT_HEADER[i])
}
fn shapes(blob: &[u8]) -> Option<Vec<AniMatteShape>> {
    if blob.get(..8)? != b"BZFX\0\0\0\0" || be32(blob, 12)? != 4 {
        return None;
    }
    let count = usize::try_from(be32(blob, 8)?).ok()?;
    if count == 0 || count > MAX_SHAPES {
        return None;
    }
    let mut at = 12usize;
    let mut total_points = 0usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let header = blob.get(at..at.checked_add(268)?)?;
        if !default_header(header) {
            return None;
        }
        let id = be32(blob, at + 4)?;
        let count = usize::try_from(be32(blob, at + 268)?).ok()?;
        total_points = total_points.checked_add(count)?;
        if !(3..=MAX_POINTS).contains(&count) || total_points > MAX_POINTS {
            return None;
        }
        at = at.checked_add(272)?;
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            let p = blob.get(at..at.checked_add(62)?)?;
            if p.get(..4)? != [0; 4] || p.get(8) != Some(&1) || !matches!(p.get(9), Some(0 | 1)) || p.get(10..14)? != [0, 0, 0, 2] {
                return None;
            }
            points.push(AniMattePoint {
                xy: [be64f(p, 14)?, be64f(p, 22)?],
                incoming: [be64f(p, 30)?, be64f(p, 38)?],
                outgoing: [be64f(p, 46)?, be64f(p, 54)?],
                corner: p[9] == 1,
            });
            at += 62;
        }
        out.push(AniMatteShape { id, points });
    }
    (at == blob.len()).then_some(out)
}

fn states(payload: &[u8], frames: u32) -> Option<Vec<AniMatteState>> {
    if frames < 2 || payload.get(..10)? != [2, 18, 255, 255, 255, 255, 0, 0, 0, 0] || payload.get(payload.len().checked_sub(2)?..)? != [1, 3] {
        return None;
    }
    let count = usize::try_from(le32(payload, 10)?).ok()?;
    if !(2..=MAX_STATES).contains(&count) {
        return None;
    }
    let mut header_at = 17usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let header = payload.get(header_at..header_at.checked_add(106)?)?;
        let timing = le32(header, 1)?;
        if timing > (1 << 30) {
            return None;
        }
        let frame = (u64::from(timing) * u64::from(frames - 1) + (1 << 29)) / (1 << 30);
        let frame = u32::try_from(frame).ok()?;
        if out.last().is_some_and(|prior: &AniMatteState| frame <= prior.frame) {
            return None;
        }
        let size_at = header_at + 106;
        let size = usize::try_from(le32(payload, size_at)?).ok()?;
        if !(12..=8 * 1024 * 1024).contains(&size) {
            return None;
        }
        let blob_at = size_at.checked_add(4)?;
        let end = blob_at.checked_add(size)?;
        let shape_state = shapes(payload.get(blob_at..end)?)?;
        out.push(AniMatteState { frame, shapes: shape_state });
        header_at = end;
    }
    if header_at != payload.len() - 2 || out.first()?.frame != 0 || out.last()?.frame != frames - 1 {
        return None;
    }
    let first_ids: Vec<_> = out.first()?.shapes.iter().map(|s| (s.id, s.points.len())).collect();
    if out.iter().any(|state| state.shapes.iter().map(|s| (s.id, s.points.len())).collect::<Vec<_>>() != first_ids) {
        return None;
    }
    Some(out)
}

fn curve_samples(payload: &[u8], frames: u32) -> Option<Vec<f64>> {
    let count = usize::try_from(le32(payload, 55)?).ok()?;
    if count != frames as usize
        || count > 8192
        || payload.get(..2)? != [2, 3]
        || payload.len() != 59usize.checked_add(count.checked_mul(22)?)?.checked_add(15)?
    {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for frame in 0..count {
        let at = 59 + frame * 22;
        if le32(payload, at)? != frame as u32
            || le32(payload, at + 4)? != 1
            || payload.get(at + 8..at + 12)? != [1, 0, 0, 0]
            || payload.get(at + 20..at + 22)? != [0, 0]
        {
            return None;
        }
        let value = le64f(payload, at + 12)?;
        if !(-4.0..=4.0).contains(&value) {
            return None;
        }
        out.push(value);
    }
    Some(out)
}
impl Bin<'_> {
    fn point_a_tracking(&self, reference: u32, shape_id: u32, frames: u32) -> Result<Option<Vec<[f64; 2]>>> {
        let Some(manager) = self.object(reference)? else { return Ok(None) };
        let p = manager.payload;
        if self.byte_order != ByteOrder::Little || manager.class != *b"TKMN" || p.len() != 11 || p.get(..2) != Some(&[2, 1]) || p.get(10) != Some(&3) {
            return Ok(None);
        }
        let Some(data) = self.object(le32(p, 2).ok_or_else(|| bad("truncated tracker manager"))?)? else { return Ok(None) };
        let Some(slots) = self.object(le32(p, 6).ok_or_else(|| bad("truncated tracker manager"))?)? else { return Ok(None) };
        let (s, d) = (slots.payload, data.payload);
        if slots.class != *b"TKPS"
            || s.len() != 47
            || s.get(..6) != Some(&[2, 1, 0x22, 0, 0x49, 0x49])
            || le32(s, 38) != Some(1)
            || s.last() != Some(&3)
            || data.class != *b"TKDS"
            || d.len() != 15
            || d.get(..2) != Some(&[2, 1])
            || le32(d, 2) != Some(1)
            || d.get(10..) != Some(&[1, 1, 0x42, 1, 3])
        {
            return Ok(None);
        }
        let Some(assignment) = self.object(le32(s, 42).ok_or_else(|| bad("truncated tracker assignment"))?)? else { return Ok(None) };
        let Some(point) = self.object(le32(d, 6).ok_or_else(|| bad("truncated tracker data"))?)? else { return Ok(None) };
        let (a, t) = (assignment.payload, point.payload);
        if assignment.class != *b"TKPA"
            || a.len() != 101
            || a.get(..6) != Some(&[2, 1, 0x60, 0, 0x49, 0x49])
            || le32(a, 14) != Some(shape_id)
            || a.get(58..60) != Some(&[8, 0])
            || a.get(60..65) != Some(&[1, 0, 0, 0, 0])
            || a.get(100) != Some(&3)
            || !(1..8).all(|i| a.get(60 + i * 5..65 + i * 5) == Some(&[0, 255, 255, 255, 255]))
            || point.class != *b"TKDA"
            || t.len() != 157
            || t.get(..6) != Some(&[2, 1, 0x38, 0, 0x49, 0x49])
            || le32(t, 10) != Some(0)
            || t.get(18..22) != Some(&[0x80, 7, 0x38, 4])
            || t.get(22..31) != Some(&[7, 0, b'P', b'o', b'i', b'n', b't', b' ', b'A'])
            || t.get(64..66) != Some(&[11, 0])
        {
            return Ok(None);
        }
        let Some(x) = self.object(le32(t, 66).ok_or_else(|| bad("truncated X curve reference"))?)? else { return Ok(None) };
        let Some(y) = self.object(le32(t, 70).ok_or_else(|| bad("truncated Y curve reference"))?)? else { return Ok(None) };
        if x.class != *b"PRCL" || y.class != *b"PRCL" {
            return Ok(None);
        }
        let (Some(xs), Some(ys)) = (curve_samples(x.payload, frames), curve_samples(y.payload, frames)) else { return Ok(None) };
        Ok(Some(xs.into_iter().zip(ys).map(|(x, y)| [x, y]).collect()))
    }
    /// Default Key In, 1920x1080 AniMatte records. Unknown layouts are deliberately not decoded.
    pub fn animatte(&self, keyframes: u32, manager: u32, frames: u32) -> Result<Option<AniMatteData>> {
        if self.byte_order != ByteOrder::Little || frames > 8192 {
            return Ok(None);
        }
        let Some(effect) = self.object(keyframes)? else { return Ok(None) };
        if effect.class != *b"FXPS" {
            return Ok(None);
        }
        let Some(states) = states(effect.payload, frames) else { return Ok(None) };
        let tracker_xy = if manager == 0 {
            None
        } else {
            if states.len() != 2 || states[0].shapes.len() != 1 || states[0].shapes != states[1].shapes {
                return Ok(None);
            }
            let Some(xy) = self.point_a_tracking(manager, states[0].shapes[0].id, frames)? else { return Ok(None) };
            Some(xy)
        };
        Ok(Some(AniMatteData { states, tracker_xy }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut blob = b"BZFX\0\0\0\0".to_vec();
        blob.extend(1u32.to_be_bytes());
        let mut header = DEFAULT_HEADER;
        header[4..8].copy_from_slice(&42u32.to_be_bytes());
        blob.extend(header);
        blob.extend(4u32.to_be_bytes());
        for id in 0..4u32 {
            blob.extend(0u32.to_be_bytes());
            blob.extend(id.to_be_bytes());
            blob.extend([1, 1, 0, 0, 0, 2]);
            for value in [id as f64, 1.0, 0.0, 0.0, 0.0, 0.0] {
                blob.extend(value.to_be_bytes());
            }
        }
        let mut effect = vec![2, 18, 255, 255, 255, 255, 0, 0, 0, 0];
        effect.extend(2u32.to_le_bytes());
        effect.extend([0; 3]);
        for time in [0u32, 1 << 30] {
            let mut prefix = [0u8; 106];
            prefix[1..5].copy_from_slice(&time.to_le_bytes());
            effect.extend(prefix);
            effect.extend((blob.len() as u32).to_le_bytes());
            effect.extend(&blob);
        }
        effect.extend([1, 3]);
        effect
    }

    #[test]
    fn bounded_default_shape_and_mutations() {
        let valid = sample();
        let parsed = states(&valid, 46).expect("two bounded states");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].shapes[0].points.len(), 4);
        for len in 0..valid.len() {
            let run = std::panic::catch_unwind(|| states(&valid[..len], 46));
            assert!(run.is_ok(), "truncated FXPS panicked at {len}");
            assert!(run.expect("checked").is_none());
        }
        for at in (0..valid.len()).step_by(17) {
            let mut changed = valid.clone();
            changed[at] ^= 0xff;
            assert!(std::panic::catch_unwind(|| states(&changed, 46)).is_ok(), "bit flip panicked at {at}");
        }
    }
}
