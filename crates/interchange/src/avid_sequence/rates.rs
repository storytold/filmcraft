//! Bounded exact-clock evidence for automatic motion adapters.
use super::*;

/// Strip only complete single-source sequences with zero-length padding, retaining frame
/// counts in their original domain until the identified MXF rate is available.
fn source_leaf<'b>(c: &'b Value, kind: u64, depth: usize, nodes: &mut usize) -> Result<&'b Value> {
    *nodes = nodes.checked_add(1).ok_or_else(|| bad("node count overflow"))?;
    if *nodes > MAX_NODES
        || depth >= 16
        || c["media_kind"] != kind
        || c.get("truncated").is_some_and(|v| v != &Value::Bool(false))
        || c.get("unsupported").is_some()
    {
        return Err(bad("incomplete or excessive motion input"));
    }
    rate(c)?;
    number(c, "length")?;
    if c["class"] == "SCLP" {
        return Ok(c);
    }
    if c["class"] != "SEQU" {
        return Err(bad("unsupported motion input structure"));
    }
    let mut found = None;
    for child in list(c, "components", 64)? {
        if child["media_kind"] != kind
            || rate(child)? != rate(c)?
            || child.get("unsupported").is_some()
            || child.get("truncated").is_some_and(|v| v != &Value::Bool(false))
        {
            return Err(bad("incomplete motion input padding"));
        }
        if child["class"] == "FILL" && number(child, "length")? == 0 {
            continue;
        }
        if found.is_some() || child["length"] != c["length"] {
            return Err(bad("motion input must be one source with zero padding"));
        }
        found = Some(source_leaf(child, kind, depth + 1, nodes)?);
    }
    found.ok_or_else(|| bad("empty motion input"))
}

/// Exact source clock for the supported constant automatic adapter. Does not infer
/// creative speed effects or certify the renderer's interpolation/cadence.
#[cfg(test)]
fn automatic_source_rate<'a>(c: &'a Value, depth: usize, nodes: &mut usize) -> Result<(&'a Value, FrameRate)> {
    automatic_source_rate_at(c, depth, nodes, rate(c)?, false)
}
pub(super) fn automatic_source_rate_at<'a>(
    c: &'a Value,
    depth: usize,
    nodes: &mut usize,
    output: FrameRate,
    approximate_phase: bool,
) -> Result<(&'a Value, FrameRate)> {
    let p = &c["properties"];
    let phase = number(p, "phase_offset")?;
    let kind = match (c["media_kind"].as_u64(), p["effect_id"].as_str()) {
        (Some(1), Some("EFF_TIMEWARP.MOTION_CTL")) => 1,
        (Some(2), Some("EFF_TIMEWARP.AUDIO_TIME_WARP")) => 2,
        _ => return Err(bad("unsupported automatic adapter kind")),
    };
    if c["class"] != "SPED"
        || c.get("truncated").is_some_and(|v| v != &Value::Bool(false))
        || c.get("unsupported").is_some_and(|v| v != "component_semantics")
        || p.pointer("/attributes/_MIXMATCH_MOTIONADAPTER") != Some(&json!(2))
        || phase > i64::from(i32::MAX)
        || (phase != 0 && !approximate_phase)
        || p["offset_adjust"].as_f64() != Some(0.0)
        || p["new_source_calculation"] != false
        || p["mc_mode"] != 0
    {
        return Err(bad("unsupported automatic adapter"));
    }
    let length = ticks_at(c, "length", output)?;
    if length <= Tick::ZERO {
        return Err(bad("empty adapter"));
    }
    let inputs = list(c, "input_tracks", 1)?;
    let input = inputs.first().filter(|i| i["index"] == 1).ok_or_else(|| bad("unsupported motion input"))?;
    let leaf = source_leaf(&input["component"], kind, depth + 1, nodes)?;
    let ratio = list(p, "speed_ratio", 2)?;
    let n = ratio.first().and_then(Value::as_i64).filter(|n| *n > 0 && *n <= i64::from(u32::MAX)).ok_or_else(|| bad("invalid adapter ratio"))?;
    let d = ratio.get(1).and_then(Value::as_i64).filter(|n| *n > 0 && *n <= i64::from(u32::MAX)).ok_or_else(|| bad("invalid adapter ratio"))?;
    let num = output.num.checked_mul(d).ok_or_else(|| bad("adapter rate overflow"))?;
    let den = output.den.checked_mul(n).ok_or_else(|| bad("adapter rate overflow"))?;
    let source = FrameRate::new(num, den);
    rate(&json!({"rate":{"numerator":source.num,"denominator":source.den}}))?;
    if !avid_catalog::picture_rate_matches(leaf, source) || number(leaf, "source_track")? == 0 {
        return Err(bad("adapter source clock disagrees with recorded source"));
    }
    let source_length = ticks(&json!({"length":leaf["length"],"rate":{"numerator":source.num,"denominator":source.den}}), "length")?;
    if source_length < length || source_length.0 - length.0 >= source.frame_duration().0 {
        return Err(bad("adapter input does not cover output duration"));
    }
    Ok((leaf, source))
}

/// Retain corroborating clocks only from complete live compositions. Conflicting
/// clocks for a master disable this fallback. Full sequence validation still runs
/// when importing sequences; this evidence authorizes only the source clock.
pub(crate) fn offline_rate_evidence(mobs: &[Value]) -> Result<BTreeMap<[u8; 32], Option<Value>>> {
    fn visit(c: &Value, depth: usize, nodes: &mut usize, out: &mut BTreeMap<[u8; 32], Option<Value>>, clock: Option<formats::Clock>) -> Result<()> {
        *nodes = nodes.checked_add(1).ok_or_else(|| bad("node count overflow"))?;
        if depth >= 16 || *nodes > MAX_NODES {
            return Err(bad("rate evidence graph exceeds bounds"));
        }
        if c.get("truncated").is_some_and(|v| v != &Value::Bool(false)) {
            return Ok(());
        }
        if c["class"] == "SPED" {
            // This index corroborates picture descriptors only; audio track numbers
            // and clocks must never become picture evidence for a combined master.
            if c["media_kind"] != 1 {
                return Ok(());
            }
            let Ok(output) = clock.map_or_else(|| rate(c), |clock| clock.rate(c)) else { return Ok(()) };
            if let Ok((leaf, r)) = automatic_source_rate_at(c, depth, nodes, output, false) {
                // This is optional corroboration, not sequence validation. Legacy
                // sources without a full identity cannot contribute a proof; the
                // converter still reports the unsupported edit in its own context.
                let Ok(id) = avid_catalog::full_identity(&leaf["source_id"]) else { return Ok(()) };
                let proof = json!({"basis":"automatic_adapter","rate":{"numerator":r.num,"denominator":r.den},"source_track":leaf["source_track"],"output_rate":{"numerator":output.num,"denominator":output.den},"recorded_output_rate":c["rate"],"speed_ratio":c["properties"]["speed_ratio"]});
                out.entry(id)
                    .and_modify(|old| {
                        if old.as_ref().is_some_and(|v| v["rate"] != proof["rate"] || v["source_track"] != proof["source_track"]) {
                            *old = None;
                        }
                    })
                    .or_insert(Some(proof));
            }
            return Ok(());
        }
        if c["class"] == "SEQU" {
            for child in list(c, "components", MAX_NODES)? {
                visit(child, depth + 1, nodes, out, clock)?;
            }
        } else if c["class"] == "TKFX" && c["properties"]["effect_id"] == "INT_EFF_SPATIAL_ADAPTER" {
            for input in list(c, "input_tracks", 1)? {
                visit(&input["component"], depth + 1, nodes, out, clock)?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    let mut nodes = 0;
    for m in mobs.iter().filter(|m| {
        m["kind"] == "CompositionMob" && m["live"] == true && m["user_placed"] == true && !m.get("truncated").is_some_and(|v| v != &Value::Bool(false))
    }) {
        let clock = match formats::resolve(m, true) {
            Ok(f) if !f.clock_conflict => Some(f.clock),
            Ok(_) => continue,
            Err(_) if m.pointer("/attributes/SEQUERNCE_FORMAT_STRING").is_some() => continue,
            Err(_) => None,
        };
        for t in list(m, "tracks", MAX_SEQUENCE_TRACKS)? {
            if !t.get("truncated").is_some_and(|v| v != &Value::Bool(false)) {
                visit(&t["component"], 0, &mut nodes, &mut out, clock)?;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn adapter() -> Value {
        json!({"class":"SPED","media_kind":1,"rate":{"numerator":24,"denominator":1},"length":46,"unsupported":"component_semantics",
            "properties":{"effect_id":"EFF_TIMEWARP.MOTION_CTL","attributes":{"_MIXMATCH_MOTIONADAPTER":2},"speed_ratio":[1001,1250],"phase_offset":0,"offset_adjust":0.0,"new_source_calculation":false,"mc_mode":0},
            "input_tracks":[{"index":1,"component":{"class":"SCLP","media_kind":1,"source_id":{"raw":"12".repeat(32),"byte_order":"big"},"source_track":1,"start":25,"length":58,"rate":{"numerator":2997,"denominator":100}}}]})
    }
    fn composition(c: Value) -> Value {
        json!({"kind":"CompositionMob","live":true,"user_placed":true,"tracks":[{"component":c}]})
    }
    #[test]
    fn exact_adapter_clock_and_nonzero_source_offset() {
        let c = adapter();
        let (leaf, r) = automatic_source_rate(&c, 0, &mut 0).unwrap();
        assert_eq!(r, FrameRate::new(30000, 1001));
        assert_eq!(leaf["start"], 25);
        // An hour of integer source frames stays on the exact tick grid.
        let frames = 108000;
        let t = ticks(&json!({"length":frames,"rate":{"numerator":r.num,"denominator":r.den}}), "length").unwrap();
        assert_eq!(t.0, TICKS_PER_SECOND * 3600 * 1001 / 1000);
        assert!(ticks(&json!({"length":frames,"rate":{"numerator":2997,"denominator":100}}), "length").is_err());
    }
    #[test]
    fn malformed_and_creative_adapters_never_authorize_a_clock() {
        let original = adapter();
        for (pointer, value) in [
            ("/rate/numerator", json!(0)),
            ("/rate/denominator", json!(0)),
            ("/properties/speed_ratio", json!([0, 1250])),
            ("/properties/speed_ratio", json!([1001, 0])),
            ("/properties/speed_ratio", json!([1001])),
            ("/properties/speed_ratio", json!([i64::MAX, 1250])),
            ("/properties/speed_ratio", json!([1000, 1250])),
            ("/properties/phase_offset", json!(1)),
            ("/properties/attributes/_MIXMATCH_MOTIONADAPTER", json!(0)),
            ("/input_tracks/0/component/length", json!(57)),
            ("/input_tracks/0/component/source_track", json!(0)),
        ] {
            let mut c = original.clone();
            *c.pointer_mut(pointer).unwrap() = value;
            assert!(std::panic::catch_unwind(|| automatic_source_rate(&c, 0, &mut 0).map(|(_, r)| r)).unwrap().is_err(), "{pointer}");
            assert!(offline_rate_evidence(&[composition(c)]).unwrap().is_empty());
        }
        let mut truncated = original.clone();
        truncated["truncated"] = json!(true);
        assert!(offline_rate_evidence(&[composition(truncated)]).unwrap().is_empty());
        assert!(automatic_source_rate(&original, 16, &mut 0).is_err());
        assert!(automatic_source_rate(&original, 0, &mut MAX_NODES.clone()).is_err());
    }
    #[test]
    fn phase_approximation_is_explicit_and_never_authorizes_offline_picture_clock() {
        let mut c = adapter();
        c["properties"]["phase_offset"] = json!(109);
        assert!(automatic_source_rate(&c, 0, &mut 0).is_err());
        let (leaf, r) = automatic_source_rate_at(&c, 0, &mut 0, FrameRate::FPS_24, true).unwrap();
        assert_eq!(leaf["start"], 25);
        assert_eq!(r, FrameRate::FPS_29_97);
        assert!(offline_rate_evidence(&[composition(c.clone())]).unwrap().is_empty());
        for phase in [json!(-1), json!(i64::MAX), Value::Null] {
            c["properties"]["phase_offset"] = phase;
            assert!(automatic_source_rate_at(&c, 0, &mut 0, FrameRate::FPS_24, true).is_err());
        }
        let mut c = adapter();
        c["media_kind"] = json!(2);
        c["properties"]["effect_id"] = json!("EFF_TIMEWARP.AUDIO_TIME_WARP");
        c["input_tracks"][0]["component"]["media_kind"] = json!(2);
        assert!(automatic_source_rate(&c, 0, &mut 0).is_ok());
        assert!(offline_rate_evidence(&[composition(c)]).unwrap().is_empty());
    }
    #[test]
    fn optional_clock_evidence_ignores_missing_identities() {
        // A legacy source without a full UMID must not prevent the other clips in
        // the bin from importing. The sequence conversion reports that failed edit.
        let valid = composition(adapter());
        for id in [Value::Null, json!({"raw":"2a0000000000000000000001","byte_order":"little"}), json!({"raw":"invalid","byte_order":"big"})] {
            let mut legacy = adapter();
            legacy["input_tracks"][0]["component"]["source_id"] = id;
            assert!(offline_rate_evidence(&[composition(legacy.clone())]).unwrap().is_empty());
            assert_eq!(offline_rate_evidence(&[composition(legacy), valid.clone()]).unwrap(), offline_rate_evidence(std::slice::from_ref(&valid)).unwrap());
        }
    }
    #[test]
    fn optional_clock_evidence_supports_sequence_track_limit() {
        // The sequence importer accepts 128 total tracks (including auxiliary tracks).
        let mut wide = composition(adapter());
        let tracks = wide["tracks"].as_array_mut().unwrap();
        tracks.extend((0..127).map(|_| json!({"component":{"class":"FILL","media_kind":3}})));
        assert_eq!(offline_rate_evidence(&[wide.clone()]).unwrap().len(), 1);
        wide["tracks"].as_array_mut().unwrap().push(json!({"component":{"class":"FILL","media_kind":3}}));
        assert!(offline_rate_evidence(&[wide]).is_err());
    }
    #[test]
    fn offline_adapter_uses_corroborated_sequence_clock_and_retains_recorded_rates() {
        let mut c = adapter();
        c["rate"] = json!({"numerator":23976,"denominator":1000});
        c["length"] = json!(48);
        c["properties"]["speed_ratio"] = json!([4, 5]);
        c["input_tracks"][0]["component"]["length"] = json!(60);
        let mut m = composition(c);
        m["rate"] = json!({"numerator":23976,"denominator":1000});
        m["attributes"] = json!({"SEQUERNCE_FORMAT_STRING":"HD 1080p/23.976"});
        let evidence = offline_rate_evidence(&[m.clone()]).unwrap();
        let proof = evidence.values().next().unwrap().as_ref().unwrap();
        assert_eq!(proof["rate"], json!({"numerator":30000,"denominator":1001}));
        assert_eq!(proof["output_rate"], json!({"numerator":24000,"denominator":1001}));
        assert_eq!(proof["recorded_output_rate"], m["rate"]);
        m["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!("Unknown raster/23.976");
        assert!(offline_rate_evidence(&[m.clone()]).unwrap().values().next().unwrap().is_some());
        m["attributes"]["SEQUERNCE_FORMAT_STRING"] = json!("HD 1080p/25");
        assert!(offline_rate_evidence(&[m]).unwrap().is_empty());
    }
    #[test]
    fn conflicting_clocks_disable_offline_fallback_and_inactive_history_is_ignored() {
        let first = composition(adapter());
        let mut other = adapter();
        other["properties"]["speed_ratio"] = json!([4, 5]);
        other["input_tracks"][0]["component"]["rate"] = json!({"numerator":30,"denominator":1});
        let other = composition(other);
        let evidence = offline_rate_evidence(&[first.clone(), other.clone(), first.clone()]).unwrap();
        assert_eq!(evidence.len(), 1);
        assert!(evidence.values().next().unwrap().is_none());
        let mut inactive = other;
        inactive["live"] = json!(false);
        assert!(offline_rate_evidence(&[first, inactive]).unwrap().values().next().unwrap().is_some());
    }
}
