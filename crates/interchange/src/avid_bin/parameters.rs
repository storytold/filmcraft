//! Bounded decoding of numeric effect parameters observed in native Avid bins.
//! Unknown parameter shapes remain opaque and cannot authorize effect translation.
use super::{Bin, ByteOrder, Cursor, bad};
use crate::Result;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct NumericParameter {
    /// Full serialized identifier; the first two bytes alone are not globally unique.
    pub id: [u8; 16],
    pub value: f64,
}

/// Source positions sampled at half-frame intervals by one observed Avid motion effect.
/// Values are source-frame offsets from the effect's child SourceClip start. The curve's
/// speed keys are deliberately not reconstructed from these samples.
#[derive(Clone, Debug, Serialize)]
pub struct TimewarpPositions {
    pub samples: Vec<f64>,
}

impl Bin<'_> {
    /// Decode only the bounded, half-frame position table used by the tested Timewarp ramps.
    /// Other parameter/curve layouts return `None` so import keeps an explicit placeholder.
    pub fn timewarp_positions(&self, reference: u32) -> Result<Option<TimewarpPositions>> {
        let Some(list) = self.object(reference)? else { return Ok(None) };
        if list.class != *b"PRLS" || list.payload.len() < 7 || list.payload[..2] != [2, 1] || list.payload.last() != Some(&3) {
            return Ok(None);
        }
        let mut c = Cursor { bytes: list.payload, at: 2, order: self.byte_order };
        let count = c.number(4)? as usize;
        if count != 3 || list.payload.len() != 7 + count * 4 {
            return Ok(None);
        }
        const POSITION_ID: [u8; 16] = [0x7d, 0x82, 0x56, 0x8d, 0x7e, 0x84, 0xd5, 0x11, 0x93, 0x5a, 0x50, 0xf8, 0x57, 0xc1, 0, 0];
        let mut position_ref = None;
        let mut seen = [false; 3];
        for _ in 0..count {
            let Some(item) = self.object(c.number(4)? as u32)? else { return Ok(None) };
            if item.class != *b"PRIT" || item.payload.len() < 36 || item.payload.get(..2) != Some(&[2, 2]) {
                return Ok(None);
            }
            let Some(id) = item.payload.get(2..18) else { return Ok(None) };
            let kind = match id.get(..2) {
                Some(&[0x83, 0x82]) => 0,
                Some(&[0x7d, 0x82]) => 1,
                Some(&[0x7c, 0x82]) => 2,
                _ => return Ok(None),
            };
            if seen[kind] || id.get(2..) != Some(&POSITION_ID[2..]) {
                return Ok(None);
            }
            seen[kind] = true;
            let scalar_len = if kind == 0 { 4 } else { 8 };
            if item.payload.len() != 32 + scalar_len
                || item.payload.get(18..20) != Some(if kind == 0 { &[1, 0] } else { &[2, 0] })
                || item.payload.get(20..20 + scalar_len).is_none_or(|bytes| bytes.iter().any(|b| *b != 0))
                || item.payload.get(20 + scalar_len..23 + scalar_len) != Some(&[0xff, 0xff, 1])
                || item.payload.get(27 + scalar_len..) != Some(&[1, 1, 0x42, 1, 3])
            {
                return Ok(None);
            }
            let Some(raw_ref) = item.payload.get(23 + scalar_len..27 + scalar_len).and_then(|b| b.try_into().ok()) else { return Ok(None) };
            let reference = u32::from_le_bytes(raw_ref);
            if kind == 1 {
                position_ref = Some(reference);
            } else if reference == 0 || self.object(reference)?.is_none_or(|target| target.class != *b"PRCL") {
                return Ok(None);
            }
        }
        if !seen.iter().all(|present| *present) {
            return Ok(None);
        }
        let Some(curve) = self.object(position_ref.unwrap_or(0))? else { return Ok(None) };
        let p = curve.payload;
        if curve.class != *b"PRCL"
            || self.byte_order != ByteOrder::Little
            || p.len() < 74
            || p.get(..2) != Some(&[2, 3])
            || p.get(34..39) != Some(&[1, 1, 0x48, 0, 0])
            || p.get(41..44) != Some(&[2, 1, 0x30])
            || p.get(47..49) != Some(&[2, 1])
            || p.get(49..53) != Some(&[3, 0, 0, 0])
            || p.get(53..55) != Some(&[2, 0])
            || p.get(p.len() - 15..) != Some(&[1, 1, 0x47, 1, 0, 0, 0, 1, 2, 0x47, 1, 0, 0, 0, 3])
        {
            return Ok(None);
        }
        let Some(count) = p.get(55..59).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes).map(|n| n as usize) else {
            return Ok(None);
        };
        if !(3..=8193).contains(&count) || count % 2 == 0 || p.len() != 59 + count * 22 + 15 {
            return Ok(None);
        }
        let mut samples = Vec::with_capacity(count);
        for i in 0..count {
            let at = 59 + i * 22;
            let Some(numerator) = p.get(at..at + 4).and_then(|b| b.try_into().ok()).map(i32::from_le_bytes) else { return Ok(None) };
            let Some(denominator) = p.get(at + 4..at + 8).and_then(|b| b.try_into().ok()).map(i32::from_le_bytes) else { return Ok(None) };
            // Avid reduces integer sample positions to n/1, while half-frame samples use n/2.
            let expected = if i % 2 == 0 { (i / 2) as i32 } else { i as i32 };
            let expected_den = if i % 2 == 0 { 1 } else { 2 };
            if numerator != expected || denominator != expected_den || p.get(at + 8..at + 12) != Some(&[1, 0, 0, 0]) || p.get(at + 20..at + 22) != Some(&[0, 0])
            {
                return Ok(None);
            }
            let Some(value) = p.get(at + 12..at + 20).and_then(|b| b.try_into().ok()).map(f64::from_le_bytes) else { return Ok(None) };
            if !value.is_finite() || !(0.0..=1_000_000.0).contains(&value) || samples.last().is_some_and(|old| value + 1e-9 < *old) {
                return Ok(None);
            }
            samples.push(value);
        }
        Ok(Some(TimewarpPositions { samples }))
    }
    /// Return `None` for an unsupported list/item shape. A bad reference or truncated list is
    /// an error. This is deliberately narrower than a general PRIT/PRCL decoder.
    pub fn numeric_parameters(&self, reference: u32) -> Result<Option<Vec<NumericParameter>>> {
        let Some(list) = self.object(reference)? else { return Ok(None) };
        if list.class != *b"PRLS" || list.payload.len() < 7 || list.payload[..2] != [2, 1] || list.payload.last() != Some(&3) {
            return Ok(None);
        }
        let mut c = Cursor { bytes: list.payload, at: 2, order: self.byte_order };
        let count = c.number(4)? as usize;
        if count > 128 || list.payload.len() != 7 + count * 4 {
            return Ok(None);
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(item) = self.object(c.number(4)? as u32)? else { return Ok(None) };
            if item.class != *b"PRIT" || item.payload.len() < 36 || item.payload[..2] != [2, 2] {
                return Ok(None);
            }
            let data = item.payload;
            let id: [u8; 16] = data[2..18].try_into().map_err(|_| bad("truncated parameter identifier"))?;
            // The four bytes after ff ff 01 are zero for the static calibration. In the
            // user-saved keyframe bin they point to PRCL curves; their scalar is not the full
            // animation. Do not flatten linked controls to a static value.
            let (size, value) = match (self.byte_order, data[18..20].try_into().map(u16::from_le_bytes)) {
                (ByteOrder::Little, Ok(1)) if data.len() == 36 => {
                    let bytes = data[20..24].try_into().map_err(|_| bad("truncated integer parameter"))?;
                    (4, f64::from(i32::from_le_bytes(bytes)))
                }
                (ByteOrder::Little, Ok(2)) if data.len() == 40 => {
                    let bytes = data[20..28].try_into().map_err(|_| bad("truncated floating parameter"))?;
                    (8, f64::from_le_bytes(bytes))
                }
                _ => return Ok(None),
            };
            let suffix = &data[20 + size..];
            if !value.is_finite() || suffix.len() != 12 || suffix[..3] != [0xff, 0xff, 1] || suffix[3..7] != [0, 0, 0, 0] || suffix[7..] != [1, 1, 0x42, 1, 3] {
                return Ok(None);
            }
            out.push(NumericParameter { id, value });
        }
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avid_bin::Object;

    fn make<'a>(list: &'a [u8], item: &'a [u8]) -> Bin<'a> {
        Bin {
            byte_order: ByteOrder::Little,
            root_index: 1,
            saved_at_raw: &[],
            creator_raw: &[],
            save_fields_raw: &[],
            reserved_raw: &[],
            objects: vec![Object { index: 1, class: *b"PRLS", offset: 0, payload: list }, Object { index: 2, class: *b"PRIT", offset: 0, payload: item }],
        }
    }

    #[test]
    fn bounded_numeric_list_requires_complete_items_and_references() {
        let mut item = vec![2, 2];
        item.extend([0xaa, 0x81, 0x56, 0x8d, 0x7e, 0x84, 0xd5, 0x11, 0x93, 0x5a, 0x50, 0xf8, 0x57, 0xc1, 0, 0]);
        item.extend(2u16.to_le_bytes());
        item.extend(100f64.to_le_bytes());
        item.extend([0xff, 0xff, 1, 0, 0, 0, 0, 1, 1, 0x42, 1, 3]);
        let mut list = vec![2, 1];
        list.extend(1u32.to_le_bytes());
        list.extend(2u32.to_le_bytes());
        list.push(3);
        let decoded = make(&list, &item).numeric_parameters(1).unwrap().unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].value, 100.0);
        list[6..10].copy_from_slice(&3u32.to_le_bytes());
        assert!(make(&list, &item).numeric_parameters(1).is_err());
        list[6..10].copy_from_slice(&2u32.to_le_bytes());
        item[18..20].copy_from_slice(&3u16.to_le_bytes());
        assert!(make(&list, &item).numeric_parameters(1).unwrap().is_none());
        item[18..20].copy_from_slice(&2u16.to_le_bytes());
        item[31..35].copy_from_slice(&833u32.to_le_bytes());
        assert!(make(&list, &item).numeric_parameters(1).unwrap().is_none());
        assert!(make(&list[..list.len() - 1], &item).numeric_parameters(1).unwrap().is_none());
    }

    #[test]
    #[ignore = "requires the locally preserved, user-authored Timewarp AVB"]
    fn saved_timewarp_position_tables() {
        let path = std::env::var("FILMCRAFT_AVID_TIMEWARP_BIN").unwrap();
        let bytes = std::fs::read(path).unwrap();
        let bin = super::super::index(&bytes).unwrap();
        let mut positions = Vec::new();
        for object in &bin.objects {
            if object.class != *b"SPED" {
                continue;
            }
            let super::super::Decoded::Group(group) = bin.decode(object.index).unwrap() else { panic!("SPED not decoded as group") };
            if group.common.effect.as_ref().is_some_and(|id| id.value.as_ref() == "EFF_ADV_MOTION_CTL") {
                let samples = bin.timewarp_positions(group.common.parameters.unwrap()).unwrap().unwrap().samples;
                positions.push(samples);
            }
        }
        assert_eq!(positions.len(), 3);
        for samples in &positions {
            assert_eq!(samples.len(), 97);
            assert_eq!(samples[0], 0.0);
        }
        assert!((positions[0][96] - 59.39984042553155).abs() < 1e-6);
        assert!((positions[1][96] - 60.60015957446746).abs() < 1e-6);
        assert!((positions[2][96] - 48.0).abs() < 1e-9);
    }

    #[test]
    fn bounded_timewarp_positions_reject_malformed_payloads_without_panicking() {
        let mut list = vec![2, 1];
        list.extend(3u32.to_le_bytes());
        list.extend(2u32.to_le_bytes());
        list.extend(4u32.to_le_bytes());
        list.extend(5u32.to_le_bytes());
        list.push(3);
        let mut item = vec![2, 2];
        item.extend([0x7d, 0x82, 0x56, 0x8d, 0x7e, 0x84, 0xd5, 0x11, 0x93, 0x5a, 0x50, 0xf8, 0x57, 0xc1, 0, 0]);
        item.extend(2u16.to_le_bytes());
        item.extend(0f64.to_le_bytes());
        item.extend([0xff, 0xff, 1]);
        item.extend(3u32.to_le_bytes());
        item.extend([1, 1, 0x42, 1, 3]);
        let mut curve = vec![0; 59];
        curve[..2].copy_from_slice(&[2, 3]);
        curve[34..39].copy_from_slice(&[1, 1, 0x48, 0, 0]);
        curve[41..44].copy_from_slice(&[2, 1, 0x30]);
        curve[47..49].copy_from_slice(&[2, 1]);
        curve[49..53].copy_from_slice(&3u32.to_le_bytes());
        curve[53..55].copy_from_slice(&2u16.to_le_bytes());
        curve[55..59].copy_from_slice(&3u32.to_le_bytes());
        for (numerator, denominator, value) in [(0i32, 1i32, 0f64), (1, 2, 0.5), (1, 1, 1.0)] {
            curve.extend(numerator.to_le_bytes());
            curve.extend(denominator.to_le_bytes());
            curve.extend(1u32.to_le_bytes());
            curve.extend(value.to_le_bytes());
            curve.extend(0u16.to_le_bytes());
        }
        curve.extend([1, 1, 0x47, 1, 0, 0, 0, 1, 2, 0x47, 1, 0, 0, 0, 3]);
        fn decode(list: &[u8], item: &[u8], curve: &[u8]) -> crate::Result<Option<TimewarpPositions>> {
            let mut mode = vec![2, 2];
            mode.extend_from_slice(&item[2..18]);
            mode[2..4].copy_from_slice(&[0x83, 0x82]);
            mode.extend(1u16.to_le_bytes());
            mode.extend(0i32.to_le_bytes());
            mode.extend([0xff, 0xff, 1]);
            mode.extend(3u32.to_le_bytes());
            mode.extend([1, 1, 0x42, 1, 3]);
            let mut speed = item.to_vec();
            speed[2..4].copy_from_slice(&[0x7c, 0x82]);
            let bin = Bin {
                byte_order: ByteOrder::Little,
                root_index: 1,
                saved_at_raw: &[],
                creator_raw: &[],
                save_fields_raw: &[],
                reserved_raw: &[],
                objects: vec![
                    Object { index: 1, class: *b"PRLS", offset: 0, payload: list },
                    Object { index: 2, class: *b"PRIT", offset: 0, payload: item },
                    Object { index: 3, class: *b"PRCL", offset: 0, payload: curve },
                    Object { index: 4, class: *b"PRIT", offset: 0, payload: &mode },
                    Object { index: 5, class: *b"PRIT", offset: 0, payload: &speed },
                ],
            };
            bin.timewarp_positions(1)
        }
        assert_eq!(decode(&list, &item, &curve).unwrap().unwrap().samples, [0.0, 0.5, 1.0]);
        for end in 0..curve.len() {
            assert!(std::panic::catch_unwind(|| decode(&list, &item, &curve[..end])).is_ok());
        }
        for at in 0..curve.len() {
            for bit in 0..8 {
                let mut damaged = curve.clone();
                damaged[at] ^= 1 << bit;
                assert!(std::panic::catch_unwind(|| decode(&list, &item, &damaged)).is_ok());
            }
        }
        for (at, replacement) in [(49, 0xff), (53, 1), (55, 0xff), (63, 0), (67, 0), (79, 1), (curve.len() - 1, 0)] {
            let mut damaged = curve.clone();
            damaged[at] = replacement;
            assert!(decode(&list, &item, &damaged).unwrap().is_none(), "offset {at}");
        }
    }
}
