//! Conservative subclip transfer: prove a linear range and preserve per-track source routing.
use super::*;

struct Source {
    parent: [u8; 32],
    start: i64,
    length: i64,
    tracks: BTreeMap<(u64, u32), u32>,
}
fn track_number(v: &Value) -> Result<u32> {
    v.as_u64().filter(|n| *n > 0 && *n <= 65535).map(|n| n as u32).ok_or_else(|| bad("invalid subclip track number"))
}
fn same_rate(a: &Value, b: &Value) -> bool {
    let rational = |v: &Value| -> Option<(i128, i128)> {
        Some((i128::from(v["numerator"].as_i64().filter(|n| *n > 0)?), i128::from(v["denominator"].as_i64().filter(|n| *n > 0)?)))
    };
    match (rational(a), rational(b)) {
        (Some((an, ad)), Some((bn, bd))) => an * bd == bn * ad,
        _ => false,
    }
}
/// Only a direct source or a chain of single-child, timing-preserving sequences is a plain range.
/// In particular, finding equal source IDs inside arbitrary effects is not proof of continuity.
fn leaf<'a>(c: &'a Value, rate: &Value, kind: u64, depth: usize) -> Result<&'a Value> {
    if depth >= 16 || c.get("unsupported").is_some() || c.get("truncated").is_some_and(|x| x != &Value::Bool(false)) {
        return Err(bad("unsupported or truncated subclip structure"));
    }
    if c["media_kind"] != kind || !same_rate(&c["rate"], rate) || c["length"].as_i64().is_none_or(|n| n <= 0) {
        return Err(bad("inconsistent subclip component timing or media kind"));
    }
    match c["class"].as_str() {
        Some("SCLP") => Ok(c),
        Some("SEQU") => {
            let children = array(c, "components")?;
            if children.len() != 1 {
                return Err(bad("subclip is not a single linear source interval"));
            }
            let child = children.first().ok_or_else(|| bad("empty subclip sequence"))?;
            if child["length"] != c["length"] {
                return Err(bad("subclip wrapper changes duration"));
            }
            leaf(child, rate, kind, depth + 1)
        }
        _ => Err(bad("unsupported subclip component")),
    }
}
fn single_source(m: &Value) -> Result<Source> {
    let tracks = array(m, "tracks")?;
    if tracks.len() > 64 {
        return Err(bad("too many subclip tracks"));
    }
    let mut source = None;
    let mut mapping = BTreeMap::new();
    let mut length = i64::MAX;
    for t in tracks {
        let c = &t["component"];
        let kind = c["media_kind"].as_u64().ok_or_else(|| bad("missing subclip media kind"))?;
        // Auxiliary timecode/edgecode do not select source media. Other unknown tracks must not
        // disappear from the proof of a simple range.
        if matches!(kind, 3 | 4) {
            continue;
        }
        if !matches!(kind, 1 | 2) {
            return Err(bad("unsupported subclip track kind"));
        }
        if t["properties"].get("start_pos").is_some_and(|v| v.as_i64() != Some(0)) {
            return Err(bad("shifted or invalid subclip track position is unsupported"));
        }
        let c = leaf(c, &m["rate"], kind, 0)?;
        length = length.min(c["length"].as_i64().ok_or_else(|| bad("missing subclip component length"))?);
        let parent = full_identity(&c["source_id"])?;
        let start = c["start"].as_i64().filter(|n| *n >= 0).ok_or_else(|| bad("invalid subclip source offset"))?;
        if source.is_some_and(|s| s != (parent, start)) {
            return Err(bad("it does not restrict exactly one master at one offset"));
        }
        source = Some((parent, start));
        if mapping.insert((kind, track_number(&t["index"])?), track_number(&c["source_track"])?).is_some() {
            return Err(bad("ambiguous subclip track mapping"));
        }
    }
    let (parent, start) = source.ok_or_else(|| bad("subclip has no media source"))?;
    Ok(Source { parent, start, length, tracks: mapping })
}
/// Read routing from retained Avid evidence, including projects saved before explicit routing
/// support. Validate again: project metadata is an input, not a trusted decoder cache.
pub(crate) fn subclip_track(m: &Value, kind: u64, track: u32) -> Result<([u8; 32], u32)> {
    let s = single_source(m)?;
    let parent_track = *s.tracks.get(&(kind, track)).ok_or_else(|| bad("missing subclip source track"))?;
    Ok((s.parent, parent_track))
}
/// Discover subclip identities referenced by placed compositions. Inspect the full bounded tree;
/// never return a successful prefix after hitting a traversal limit.
pub(super) fn referenced_subclips(mobs: &[Value]) -> Result<BTreeSet<[u8; 32]>> {
    fn walk(v: &Value, out: &mut BTreeSet<[u8; 32]>, remaining: &mut usize, depth: usize) -> Result<()> {
        *remaining = remaining.checked_sub(1).ok_or_else(|| bad("subclip dependency budget exceeded"))?;
        if depth >= 64 {
            return Err(bad("subclip dependencies exceed depth limit"));
        }
        match v {
            Value::Object(o) => {
                if o.get("class").and_then(Value::as_str) == Some("SCLP")
                    && let Some(id) = o.get("source_id").and_then(|v| full_identity(v).ok())
                {
                    out.insert(id);
                }
                for child in o.values() {
                    walk(child, out, remaining, depth + 1)?;
                }
            }
            Value::Array(a) => {
                for child in a {
                    walk(child, out, remaining, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut out = BTreeSet::new();
    let mut remaining = 4_000_000;
    for m in mobs.iter().filter(|m| m["kind"] == "CompositionMob" && m["user_placed"] == true) {
        walk(&m["tracks"], &mut out, &mut remaining, 0)?;
    }
    Ok(out)
}

/// Where a subclip's range actually lives. Avid writes it two ways, and reading either alone is
/// wrong somewhere: with `_START`/`_END` present the component says "the whole master" and the
/// mob's length is the master's, while elsewhere the offset is on the component and the duration
/// is the mob's length. Measured against a fixture whose in and out points were set deliberately.
fn subclip_range(m: &Value, component_start: i64) -> Result<(Tick, Tick)> {
    let attrs = m.get("attributes");
    let start_attr = attrs.and_then(|a| a.get("_START")).and_then(Value::as_i64).filter(|n| *n >= 0);
    let end_attr = attrs.and_then(|a| a.get("_END")).and_then(Value::as_i64).filter(|n| *n >= 0);
    let (offset, span) = match (start_attr, end_attr) {
        // Attributes present: the offset is `_START` and the span `_END` - `_START`. A zero
        // `_START` means the component carries the offset instead.
        (Some(a), end) => (if a > 0 { a } else { component_start }, end.filter(|e| *e > a).map(|e| e - a)),
        (None, _) => (component_start, None),
    };
    if offset < 0 {
        return Err(bad("invalid subclip offset"));
    }
    let rate = m.get("rate").ok_or_else(|| bad("subclip has no rate"))?;
    let to_tick = |frames: i64| -> Result<Tick> { Ok(duration(&serde_json::json!({"length":frames,"rate":rate}))?.0) };
    let span = match span {
        Some(n) if n > 0 => n,
        // Without a usable span the mob's own length is the duration — but only where that length
        // describes the subclip. In the attribute form it is the master's, so an offset with no
        // span is refused rather than silently stretched to the end of the master.
        _ if offset > 0 && start_attr.is_some_and(|a| a > 0) => return Err(bad("it records a start without a usable end")),
        _ => m.get("length").and_then(Value::as_i64).filter(|n| *n > 0).ok_or_else(|| bad("it has no usable range"))?,
    };
    Ok((to_tick(offset)?, to_tick(span)?))
}
/// Decide one placed or referenced usage-2 composition. Anything that is not a plain range of one available
/// master is reported by name rather than approximated.
pub(super) fn subclip(m: &Value, available: &BTreeMap<[u8; 32], Tick>) -> Result<(SubclipPlan, bool)> {
    if m.get("live") == Some(&Value::Bool(false)) {
        return Err(bad("the bin marks it inactive"));
    }
    let source = single_source(m)?;
    let (parent, component_start) = (source.parent, source.start);
    let extent = *available.get(&parent).ok_or_else(|| bad("its master is not in this bin"))?;
    let name = text(m, "name")?.to_owned();
    let (start, mut duration) = subclip_range(m, component_start)?;
    if duration <= Tick::ZERO {
        return Err(bad("its range is empty"));
    }
    // A subclip cannot exceed what it restricts. Recorded ranges do run past a master's end, by a
    // frame or by dozens, so bound it here rather than build an item reaching past its media.
    let end = start.0.checked_add(duration.0).ok_or_else(|| bad("its range overflows"))?;
    let clamped = end > extent.0;
    if clamped {
        if start >= extent {
            return Err(bad("it starts after the end of its master"));
        }
        duration = Tick(extent.0 - start.0);
    }
    let component_begin = super::duration(&serde_json::json!({"length":component_start,"rate":m["rate"]}))?.0;
    if start < component_begin {
        return Err(bad("subclip range starts before its source component"));
    }
    let component_end = component_start.checked_add(source.length).ok_or_else(|| bad("subclip component extent overflows"))?;
    let component_end = super::duration(&serde_json::json!({"length":component_end,"rate":m["rate"]}))?.0;
    if start.0.checked_add(duration.0).is_none_or(|end| end > component_end.0) {
        return Err(bad("subclip range exceeds its source component"));
    }
    Ok((SubclipPlan { name, placed: membership(m)?, parent, start, duration, evidence: m.clone() }, clamped))
}
