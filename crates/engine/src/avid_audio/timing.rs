//! Narrow full-source audio clock reconciliation, backed by identified MXF sample ranges.
//! Decimal labels alone never authorize this interpretation, nor does stored essence padding.
use super::*;
use crate::component_relink::IdentityIndex;
use filmcraft_media::MediaSource;
use std::collections::BTreeMap;

pub(super) struct Reconciled {
    pub duration: Tick,
    pub lengths: BTreeMap<u32, Tick>,
    pub evidence: Value,
}
fn rate_is(value: &Value, numerator: i64, denominator: i64) -> bool {
    let n = value["rate"]["numerator"].as_i64().filter(|n| *n > 0);
    let d = value["rate"]["denominator"].as_i64().filter(|n| *n > 0);
    matches!((n, d), (Some(n), Some(d)) if i128::from(n) * i128::from(denominator) == i128::from(d) * i128::from(numerator))
}
fn duration(frames: i64) -> Result<Tick> {
    let units = i128::from(frames) * 1001 * i128::from(TICKS_PER_SECOND) / 24000;
    if frames <= 0 || units > i128::from(Tick::MAX.0) {
        return Err(bad("audio master duration exceeds supported range"));
    }
    Ok(Tick(i64::try_from(units).map_err(|_| bad("audio duration overflow"))?))
}
/// The declared range must equal the whole samples within the candidate full-master interval.
/// Extra stored samples do not participate. We do not clamp arbitrary overlong source ranges.
fn corroborated_length(master: Tick, declared: Tick, sample_rate: u32) -> Option<Tick> {
    if master <= Tick::ZERO || declared <= Tick::ZERO || !(8000..=384000).contains(&sample_rate) {
        return None;
    }
    let declared_scaled = i128::from(declared.0) * i128::from(sample_rate);
    if declared_scaled % i128::from(TICKS_PER_SECOND) != 0 {
        return None;
    }
    let expected_samples = i128::from(master.0) * i128::from(sample_rate) / i128::from(TICKS_PER_SECOND);
    (expected_samples == declared_scaled / i128::from(TICKS_PER_SECOND)).then_some(declared)
}
pub(super) fn reconcile(clip: &Value, index: Option<&IdentityIndex>, services: &dyn Services) -> Result<Option<Reconciled>> {
    let Some(index) = index else { return Ok(None) };
    if !rate_is(clip, 23976, 1000) {
        return Ok(None);
    }
    let Some(frames) = clip["length"].as_i64().filter(|n| *n > 0) else { return Ok(None) };
    let master = duration(frames)?;
    let Some(tracks) = clip["tracks"].as_array().filter(|t| !t.is_empty() && t.len() <= 64) else { return Ok(None) };
    let mut lengths = BTreeMap::new();
    let mut proofs = Vec::new();
    for t in tracks {
        let leaf = direct_source(&t["component"])?;
        let Some(refs) = t["references"].as_array().filter(|r| r.len() == 1) else { return Ok(None) };
        let r = &refs[0];
        let multiplier = if rate_is(r, 23976, 1000) {
            1
        } else if rate_is(r, 95904, 1000) {
            4
        } else {
            return Ok(None);
        };
        let expected = frames.checked_mul(multiplier).ok_or_else(|| bad("audio edit-unit count overflow"))?;
        if leaf["media_kind"] != 2
            || leaf["start"] != 0
            || r["source_start"] != 0
            || r["length"].as_i64() != Some(expected)
            || leaf["length"] != r["length"]
            || leaf["rate"] != r["rate"]
            || r["managed_descriptor_verified"] != true
        {
            return Ok(None);
        }
        let package = full_identity(&r["source_id"]).map_err(|e| bad(e.to_string()))?;
        if full_identity(&leaf["source_id"]).ok() != Some(package) {
            return Err(bad("audio source identity disagrees with resolved reference"));
        }
        let Some(path) = index.unique_path(package) else { return Ok(None) };
        let source = component_audio::inspect_mxf(Some(path), package, filmcraft_mxf::TrackKind::Sound, Tick::ZERO, Tick::ZERO, services)?;
        let info = source.info();
        let Some(audio) = info.audio().filter(|a| a.channels == 1 && a.codec == "PCM") else { return Ok(None) };
        let Some(essence) = source.essence_tracks().first() else { return Ok(None) };
        let declared = essence.duration.filter(|n| *n > 0);
        let backed = declared.and_then(|n| u64::try_from(n).ok()).is_some_and(|n| n <= essence.stored_sample_frames());
        if !backed
            || essence.origin != 0
            || essence.start_position != 0
            || i64::from(essence.edit_rate.num) != i64::from(audio.sample_rate)
            || essence.edit_rate.den != 1
            || declared.is_none_or(|n| Tick::from_units(n, i64::from(audio.sample_rate)) != info.duration)
        {
            return Ok(None);
        }
        let Some(length) = corroborated_length(master, info.duration, audio.sample_rate) else { return Ok(None) };
        let track = t["index"].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0).ok_or_else(|| bad("invalid audio track ID"))?;
        if lengths.insert(track, length).is_some() {
            return Err(bad("duplicate audio track ID"));
        }
        proofs.push(json!({"track":track,"file_package":r["source_id"],"recorded_rate":r["rate"],
            "interpreted_rate":{"numerator":24000 * multiplier,"denominator":1001},"length_units":r["length"],
            "sample_rate":audio.sample_rate,"declared_duration_ticks":length.0,
            "endpoint_fraction_ticks":master.0-length.0}));
    }
    Ok(Some(Reconciled {
        duration: master,
        lengths,
        evidence: json!({
            "basis":"avid_nominal_clock_and_identified_mxf_declared_samples","recorded_master_rate":clip["rate"],
            "master_rate":{"numerator":24000,"denominator":1001},"master_frames":frames,"components":proofs
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_source_endpoints_use_declared_samples_not_padding_or_tolerance() {
        let master = duration(99).unwrap();
        assert_eq!(corroborated_length(master, Tick::from_units(198198, 48000), 48000), Some(master));
        let master = duration(48).unwrap();
        let declared = Tick::from_units(88288, 44100);
        assert_eq!(corroborated_length(master, declared, 44100), Some(declared));
        assert!(master > declared);
        // Neither stored padding nor a one-sample-short file establishes the full range.
        assert!(corroborated_length(master, Tick::from_units(88289, 44100), 44100).is_none());
        assert!(corroborated_length(master, Tick::from_units(88287, 44100), 44100).is_none());
        assert!(corroborated_length(master, Tick(declared.0 + 1), 44100).is_none());
        assert!(corroborated_length(master, declared, 0).is_none());
        assert!(duration(i64::MAX).is_err());
        assert!(duration(-1).is_err());
    }
}
