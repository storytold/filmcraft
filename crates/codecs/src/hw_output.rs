//! Header-derived output obligations; unknown syntax stays unknown, never inferred from missing PTS.

use std::collections::BTreeMap;
use std::sync::Arc;

use filmcraft_bitstream::unescape_rbsp;

use crate::hw::{FrameCodec, NalCodec, StreamInfo, without_trailing_zeros};

/// Current input's display intent, not proof of payload validity or hardware completion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SampleOutput {
    /// Intended displayed pictures in this complete input.
    pub pictures: usize,
    /// Replay restart candidate, subject to the preceding-reference rule.
    pub restart: bool,
    /// CRA leading pictures require the previous restart's references.
    pub keep_previous_restart: bool,
    /// IDR syntax may discard older DPB output; unresolved publication needs software interpretation.
    pub may_discard_prior: bool,
}

enum Parameters {
    H264 { sps: Vec<Option<filmcraft_h264::params::Sps>>, pps: BTreeMap<u32, filmcraft_h264::params::Pps> },
    Hevc { sps: BTreeMap<u32, Arc<filmcraft_hevc::params::Sps>>, pps: BTreeMap<u32, Arc<filmcraft_hevc::params::Pps>> },
}

struct RunState {
    first_picture: bool,
    skip_rasl: bool,
}

impl Default for RunState {
    fn default() -> Self {
        Self { first_picture: true, skip_rasl: false }
    }
}

/// Shared bounded header inspection. Parameter changes or unknown output rules require software.
pub struct OutputInspector {
    info: StreamInfo,
    parameters: Option<Parameters>,
    byte_limit: usize,
    run: Option<RunState>,
}

impl OutputInspector {
    /// byte_limit: caller's replay-input allocation bound; also bounds parameter/header copies.
    pub fn new(info: StreamInfo, byte_limit: usize) -> Self {
        let parameters = Self::parameters(&info, byte_limit);
        Self { info, parameters, byte_limit, run: Some(RunState::default()) }
    }

    /// Configured stream shared by selection and output inspection.
    pub fn stream(&self) -> &StreamInfo {
        &self.info
    }

    /// Forget run state after seek; retain the sample-entry configuration.
    pub fn reset(&mut self) {
        self.run = Some(RunState::default());
    }

    fn parameters(info: &StreamInfo, byte_limit: usize) -> Option<Parameters> {
        let StreamInfo::Nal(info) = info else { return None };
        let total = info.parameter_sets.iter().try_fold(0usize, |sum, nal| sum.checked_add(nal.len()))?;
        if total > byte_limit {
            return None;
        }
        match info.codec {
            NalCodec::H264 => {
                // H.264 7.4.2.1: seq_parameter_set_id is 0..31; PPS parsing resolves that fixed table.
                let mut sps = vec![None; 32];
                let mut pps = BTreeMap::new();
                for nal in &info.parameter_sets {
                    if nal.first().is_some_and(|h| h & 0x1f == 7) {
                        let parsed = filmcraft_h264::params::Sps::parse(&unescape_rbsp(nal.get(1..)?)).ok()?;
                        let id = parsed.id as usize;
                        *sps.get_mut(id)? = Some(parsed);
                    }
                }
                for nal in &info.parameter_sets {
                    if nal.first().is_some_and(|h| h & 0x1f == 8) {
                        let parsed = filmcraft_h264::params::Pps::parse(&unescape_rbsp(nal.get(1..)?), &sps).ok()?;
                        pps.insert(parsed.id, parsed);
                    }
                }
                Some(Parameters::H264 { sps, pps })
            }
            NalCodec::Hevc => {
                let mut sps = BTreeMap::new();
                let mut pps = BTreeMap::new();
                for nal in &info.parameter_sets {
                    let header = filmcraft_hevc::slice::NalHeader::parse(nal).ok()?;
                    match header.nal_type {
                        33 => {
                            let parsed = filmcraft_hevc::params::Sps::parse(&unescape_rbsp(nal.get(2..)?)).ok()?;
                            sps.insert(parsed.id, Arc::new(parsed));
                        }
                        34 => {
                            let parsed = filmcraft_hevc::params::Pps::parse(&unescape_rbsp(nal.get(2..)?)).ok()?;
                            pps.insert(parsed.id, Arc::new(parsed));
                        }
                        _ => {}
                    }
                }
                Some(Parameters::Hevc { sps, pps })
            }
        }
    }

    /// Whole-input inspection. None invalidates the run until reset; includes unsupported/ambiguous syntax.
    pub fn inspect(&mut self, sample: &[u8]) -> Option<SampleOutput> {
        let mut run = self.run.take()?;
        let inspected = self.inspect_run(sample, &mut run);
        if inspected.is_some() {
            self.run = Some(run);
        }
        inspected
    }

    fn inspect_run(&self, sample: &[u8], run: &mut RunState) -> Option<SampleOutput> {
        if sample.len() > self.byte_limit {
            return None;
        }
        match &self.info {
            StreamInfo::Frame(info) if info.codec == FrameCodec::Vp9 => {
                if info.parameters_changed(sample) {
                    return None;
                }
                Some(SampleOutput { pictures: filmcraft_vp9::display_count(sample).ok()?, restart: info.is_random_access(sample), ..Default::default() })
            }
            StreamInfo::Frame(_) => None,
            StreamInfo::Nal(info) => {
                let mut output = SampleOutput::default();
                let mut h264_first = None;
                let mut hevc_last = None;
                visit_nals(sample, info.length_size, |nal| {
                    match info.codec {
                        NalCodec::H264 => {
                            use filmcraft_h264::slice::{NalHeader, SliceHeader, is_new_picture};
                            let header = NalHeader::parse(*nal.first()?).ok()?;
                            if info.is_parameter_set(header.nal_unit_type)
                                && !info.parameter_sets.iter().any(|configured| without_trailing_zeros(configured) == without_trailing_zeros(nal))
                            {
                                return None;
                            }
                            match header.nal_unit_type {
                                1 | 5 => {
                                    let Some(Parameters::H264 { sps, pps }) = &self.parameters else { return None };
                                    let rbsp = unescape_rbsp(nal.get(1..)?);
                                    let (sh, p, s) = SliceHeader::parse(&rbsp, header, |id| {
                                        let p = pps.get(&id).ok_or_else(|| filmcraft_h264::Error::MissingParameterSet(format!("PPS {id}")))?;
                                        let s = sps
                                            .get(p.sps_id as usize)
                                            .and_then(Option::as_ref)
                                            .ok_or_else(|| filmcraft_h264::Error::MissingParameterSet(format!("SPS {}", p.sps_id)))?;
                                        Ok((p, s))
                                    })
                                    .ok()?;
                                    // Whole primary pictures only; FMO header lengths use an approximate change rate.
                                    if p.num_slice_groups > 1 || s.separate_colour_plane || sh.field_pic || sh.redundant_pic_cnt != 0 {
                                        return None;
                                    }
                                    if let Some(first) = &h264_first {
                                        if sh.first_mb_in_slice == 0 || is_new_picture(first, &sh, s) {
                                            return None;
                                        }
                                    } else {
                                        if sh.first_mb_in_slice != 0 {
                                            return None;
                                        }
                                        output.pictures = 1;
                                        output.restart = sh.idr;
                                        output.may_discard_prior = sh.idr && sh.no_output_of_prior_pics;
                                        h264_first = Some(sh);
                                    }
                                }
                                6..=13 => {}
                                _ => return None,
                            }
                        }
                        NalCodec::Hevc => {
                            use filmcraft_hevc::slice::{NalHeader, SliceHeader};
                            let header = NalHeader::parse(nal).ok()?;
                            if info.is_parameter_set(header.nal_type)
                                && !info.parameter_sets.iter().any(|configured| without_trailing_zeros(configured) == without_trailing_zeros(nal))
                            {
                                return None;
                            }
                            if header.layer_id != 0 || matches!(header.nal_type, 36 | 37) {
                                // EOS selects software; this inspector never continues a post-EOS hardware run.
                                return None;
                            }
                            if header.skips_rasl(run.first_picture, run.skip_rasl) {
                                return Some(());
                            }
                            if header.is_vcl() {
                                let Some(Parameters::Hevc { sps, pps }) = &self.parameters else { return None };
                                let rbsp = unescape_rbsp(nal.get(2..)?);
                                let (sh, _, s) = SliceHeader::parse(
                                    &rbsp,
                                    header,
                                    |id| {
                                        let p = pps.get(&id).ok_or_else(|| filmcraft_hevc::Error::MissingParameterSet(format!("PPS {id}")))?;
                                        let s = sps.get(&p.sps_id).ok_or_else(|| filmcraft_hevc::Error::MissingParameterSet(format!("SPS {}", p.sps_id)))?;
                                        Ok((p.clone(), s.clone()))
                                    },
                                    hevc_last.as_ref(),
                                )
                                .ok()?;
                                // Separate planes do not prove a complete displayed picture in this input.
                                if s.separate_colour_plane || sh.data_offset >= rbsp.len() {
                                    return None;
                                }
                                if let Some(last) = &hevc_last {
                                    if sh.first_slice_segment_in_pic
                                        || sh.pps_id != last.pps_id
                                        || sh.poc_lsb != last.poc_lsb
                                        || sh.pic_output != last.pic_output
                                    {
                                        return None;
                                    }
                                } else {
                                    if !sh.first_slice_segment_in_pic {
                                        return None;
                                    }
                                    output.pictures = usize::from(sh.pic_output);
                                    output.restart = header.is_irap();
                                    output.keep_previous_restart = header.nal_type == 21;
                                    output.may_discard_prior = header.is_idr() && sh.no_output_of_prior_pics;
                                    if header.is_irap() {
                                        run.skip_rasl = header.no_rasl_output(run.first_picture, false);
                                    }
                                    run.first_picture = false;
                                }
                                if !sh.dependent {
                                    hevc_last = Some(sh);
                                }
                            } else if !matches!(header.nal_type, 32..=35 | 38..=40) {
                                return None;
                            }
                        }
                    }
                    Some(())
                })?;
                Some(output)
            }
        }
    }
}

fn visit_nals(sample: &[u8], length_size: usize, mut visit: impl FnMut(&[u8]) -> Option<()>) -> Option<()> {
    if sample.is_empty() || !(1..=4).contains(&length_size) {
        return None;
    }
    let mut pos = 0usize;
    while pos < sample.len() {
        let end = pos.checked_add(length_size)?;
        let size = sample.get(pos..end)?.iter().fold(0usize, |size, &b| (size << 8) | usize::from(b));
        if size == 0 {
            return None;
        }
        pos = end.checked_add(size)?;
        visit(sample.get(end..pos)?)?;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nal_accounting_requires_complete_nonempty_input() {
        let mut seen = Vec::new();
        assert_eq!(
            visit_nals(&[1, 9, 1, 12], 1, |nal| {
                seen.push(nal[0]);
                Some(())
            }),
            Some(())
        );
        assert_eq!(seen, [9, 12]);
        for malformed in [&[][..], &[0][..], &[2, 9][..], &[1, 9, 2, 12][..]] {
            assert_eq!(visit_nals(malformed, 1, |_| Some(())), None);
        }
        assert_eq!(visit_nals(&[1, 9], 0, |_| Some(())), None);
        assert_eq!(visit_nals(&[0xff; 4], 4, |_| Some(())), None);
    }
}
