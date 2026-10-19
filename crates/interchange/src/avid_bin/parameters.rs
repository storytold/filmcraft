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

impl Bin<'_> {
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
}
