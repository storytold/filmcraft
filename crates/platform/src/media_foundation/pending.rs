//! Bounded input metadata retained until native output is converted successfully.

use crate::biplanar::Geometry;

/// A DPB holds at most 16 pictures; a larger bounded window also permits driver latency.
const MAX_IN_FLIGHT: usize = 64;

#[derive(Clone, Copy)]
pub(super) struct PendingPicture {
    pub pts: i64,
    pub geometry: Geometry,
}

#[derive(Default)]
pub(super) struct PendingPictures {
    pictures: Vec<PendingPicture>,
}

fn reserve_picture(pictures: &mut Vec<PendingPicture>) -> Result<(), String> {
    pictures.try_reserve(1).map_err(|e| format!("cannot reserve native input metadata: {e}"))
}

impl PendingPictures {
    pub(super) fn len(&self) -> usize {
        self.pictures.len()
    }

    pub(super) fn clear(&mut self) {
        self.pictures.clear();
    }

    pub(super) fn submit(&mut self, pts: i64, geometry: Geometry, input: impl FnOnce() -> Result<bool, String>) -> Result<bool, String> {
        self.submit_with_reservation(pts, geometry, reserve_picture, input)
    }

    fn submit_with_reservation(
        &mut self,
        pts: i64,
        geometry: Geometry,
        reserve: impl FnOnce(&mut Vec<PendingPicture>) -> Result<(), String>,
        input: impl FnOnce() -> Result<bool, String>,
    ) -> Result<bool, String> {
        if self.pictures.len() >= MAX_IN_FLIGHT {
            return Err(format!("the decoder holds {} pictures back", self.pictures.len()));
        }
        reserve(&mut self.pictures)?;
        if self.pictures.capacity() <= self.pictures.len() {
            return Err("no reserved native input metadata slot".into());
        }
        if !input()? {
            return Ok(false);
        }
        // Capacity was reserved before ProcessInput; successful native input cannot lose its
        // interpretation to an allocation error afterward.
        self.pictures.push(PendingPicture { pts, geometry });
        Ok(true)
    }

    fn output_index(&self, stamp: i64) -> Result<usize, String> {
        self.pictures
            .iter()
            .position(|p| p.pts == stamp)
            // Keep the existing earliest-PTS rule for driver restamps, selecting its geometry
            // too. This cannot resolve arbitrary undocumented MFT reorder/restamp ambiguity.
            .or_else(|| self.pictures.iter().enumerate().min_by_key(|(_, p)| p.pts).map(|(i, _)| i))
            .ok_or_else(|| "native output has no submitted picture metadata".into())
    }

    pub(super) fn picture_for_output(&self, stamp: i64) -> Result<PendingPicture, String> {
        let index = self.output_index(stamp)?;
        self.pictures.get(index).copied().ok_or_else(|| "native output metadata index is out of bounds".into())
    }

    pub(super) fn convert_output<T>(&mut self, stamp: i64, convert: impl FnOnce(&Geometry) -> Result<T, String>) -> Result<(i64, T), String> {
        let index = self.output_index(stamp)?;
        let picture = self.pictures.get(index).copied().ok_or("native output metadata index is out of bounds")?;
        let frame = convert(&picture.geometry)?;
        // The index was checked above and convert cannot mutate this queue. Preserve submission
        // order so duplicate timestamps keep their earliest matching metadata record.
        self.pictures.remove(index);
        Ok((picture.pts, frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biplanar::{self, Biplanar, Geometry};
    use filmcraft_color::{ColorInfo, Matrix, Primaries, Range, Transfer};

    fn geometry(transfer: Transfer) -> Geometry {
        Geometry {
            crop: (0, 0, 2, 2),
            bits: 10,
            color: ColorInfo { primaries: Primaries::Bt2020, transfer, matrix: Matrix::Bt2020Ncl, range: Range::Limited },
            par: (1, 1),
        }
    }

    #[test]
    fn mf_pending_exact_duplicate_and_restamped_pts_keep_each_inputs_geometry() {
        let mut pending = PendingPictures::default();
        for (pts, transfer) in [(12, Transfer::Pq), (12, Transfer::Hlg), (3, Transfer::Bt709)] {
            assert!(pending.submit(pts, geometry(transfer), || Ok(true)).unwrap());
        }
        let (pts, color) = pending.convert_output(12, |g| Ok(g.color)).unwrap();
        assert_eq!((pts, color.transfer), (12, Transfer::Pq));
        let (pts, color) = pending.convert_output(999, |g| Ok(g.color)).unwrap();
        assert_eq!((pts, color.transfer), (3, Transfer::Bt709));
        let (pts, color) = pending.convert_output(12, |g| Ok(g.color)).unwrap();
        assert_eq!((pts, color.transfer), (12, Transfer::Hlg));
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn mf_pending_output_conversion_error_retains_the_same_metadata_for_replay() {
        let mut pending = PendingPictures::default();
        pending.submit(-7, geometry(Transfer::Pq), || Ok(true)).unwrap();
        assert!(pending.convert_output(-7, |_| Err::<(), _>("readback failed".to_string())).is_err());
        assert_eq!(pending.len(), 1);
        let luma = [0; 8];
        let chroma = [0; 4];
        let (pts, frame) = pending.convert_output(-7, |g| biplanar::to_frame(&Biplanar { luma: &luma, chroma: &chroma, stride: 4, rows: 2 }, g)).unwrap();
        assert_eq!((pts, frame.color.transfer), (-7, Transfer::Pq));
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn mf_pending_capacity_and_reservation_failure_reject_before_native_input() {
        let mut pending = PendingPictures::default();
        for pts in 0..64 {
            pending.submit(pts, geometry(Transfer::Pq), || Ok(true)).unwrap();
        }
        assert!(pending.submit(65, geometry(Transfer::Hlg), || panic!("full queue cannot submit native input")).is_err());
        pending.clear();
        assert!(
            pending
                .submit_with_reservation(
                    0,
                    geometry(Transfer::Pq),
                    |_| Err("metadata allocation failed".into()),
                    || panic!("failed reserve cannot submit native input")
                )
                .is_err()
        );
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn mf_pending_rejected_or_failed_input_does_not_claim_a_picture() {
        let mut pending = PendingPictures::default();
        assert!(!pending.submit(1, geometry(Transfer::Pq), || Ok(false)).unwrap());
        assert!(pending.submit(2, geometry(Transfer::Hlg), || Err("ProcessInput failed".into())).is_err());
        assert_eq!(pending.len(), 0);
        assert!(pending.convert_output::<()>(1, |_| panic!("unassociated output cannot be converted")).is_err());
    }
}
