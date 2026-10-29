use super::{ClipId, EffectInstance, FilmcraftApp, LayerView, ParamValue, Session, Tick, graphic, layer_display_name, layer_indices};

pub(super) fn layer_display_name_view(app: &FilmcraftApp, v: &LayerView) -> String {
    app.session
        .active_sequence()
        .and_then(|q| q.find_item(v.clip))
        .and_then(|(_, it)| layer_indices(&it.effects).get(v.layer).map(|&e| layer_display_name(&it.effects[e], v.layer)))
        .unwrap_or_default()
}

/// The effect instance of layer `layer` of `clip` when it is enabled and on screen.
///
/// This is the read-only path for the IME producer: it never evaluates a `LayerSpec` (which
/// clones the layer's full text) just to read an identity or a committed-text window.
fn current_layer_effect(session: &Session, clip: u64, layer: usize) -> Option<&EffectInstance> {
    let seq = session.active_sequence()?;
    let (_, it) = seq.find_item(ClipId(clip))?;
    if !it.enabled {
        return None;
    }
    let &effect_index = layer_indices(&it.effects).get(layer)?;
    let effect = it.effects.get(effect_index)?;
    effect.enabled.then_some(effect)
}

/// Borrowed committed text of the edited layer at the playhead (no `TextProps`/full-text copy).
pub(super) fn current_layer_text(session: &Session, clip: u64, layer: usize) -> Option<&str> {
    let seq = session.active_sequence()?;
    let t = session.playhead();
    let (_, it) = seq.find_item(ClipId(clip))?;
    if !it.enabled {
        return None;
    }
    let &effect_index = layer_indices(&it.effects).get(layer)?;
    let effect = it.effects.get(effect_index)?;
    if !effect.enabled || effect.effect != graphic::TEXT_LAYER {
        return None;
    }
    effect.params.get("text")?.text_ref_at(it.source_time_at(t))
}

/// Stable uid of the enabled layer at index `layer`, or 0 when that layer has no uid; `None`
/// when the clip, item or layer is missing or disabled.
pub(super) fn current_layer_uid(session: &Session, clip: u64, layer: usize) -> Option<u64> {
    current_layer_effect(session, clip, layer).map(|effect| effect.layer.as_ref().map_or(0, |x| x.uid))
}

/// A compact identity for a layer that survives reorder/delete/undo when the layer has no uid.
///
/// It uses static parameter values only, so a keyframed or animating position at the playhead
/// does not make the target look new, and it deliberately excludes the editable text, so typing
/// does not either.
pub(super) fn layer_fingerprint(effect: &EffectInstance) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut hash = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    hash(effect.effect.as_bytes());
    hash(&[u8::from(effect.enabled)]);
    hash(&effect.layer.as_ref().map_or(0, |x| x.uid).to_le_bytes());
    if let Some(name) = effect.params.get("name").and_then(|p| match &p.value {
        ParamValue::Text(s) => Some(s.as_str()),
        _ => None,
    }) {
        hash(name.as_bytes());
    }
    for id in ["position", "size", "box_width", "box_height", "shape", "sides", "corner_radius", "vertical"] {
        let Some(value) = effect.params.get(id).map(|p| &p.value) else { continue };
        match value {
            ParamValue::Float(v) => hash(&v.to_bits().to_le_bytes()),
            ParamValue::Vec2(v) => {
                hash(&v.x.to_bits().to_le_bytes());
                hash(&v.y.to_bits().to_le_bytes());
            }
            ParamValue::Choice(v) => hash(&u64::from(*v).to_le_bytes()),
            ParamValue::Bool(v) => hash(&[u8::from(*v)]),
            ParamValue::Text(s) => hash(s.as_bytes()),
            _ => {}
        }
    }
    h
}

/// Static-identity fingerprint of the enabled layer at `layer`, or `None` when it is not
/// present. Editable text is excluded by [`layer_fingerprint`], so typing does not change it.
pub(super) fn current_layer_fingerprint(session: &Session, clip: u64, layer: usize) -> Option<u64> {
    current_layer_effect(session, clip, layer).map(layer_fingerprint)
}

/// Stable uid and identity fingerprint of the layer currently at `layer`.
///
/// A control request that explicitly names a clip and numeric layer means that index now, not
/// whichever layer an older edit session happened to own at that index.
pub(crate) fn current_layer_identity(session: &Session, clip: u64, layer: usize) -> Option<(u64, u64)> {
    current_layer_effect(session, clip, layer).map(|effect| (effect.layer.as_ref().map_or(0, |extra| extra.uid), layer_fingerprint(effect)))
}

/// Properties that change a text or shape layer's layout, and therefore the caret and candidate
/// rectangle, without necessarily changing its committed text or stable identity. Both static
/// values and keyframes at the playhead are included.
const LAYOUT_PARAMS: &[&str] = &[
    // Text layout.
    "font",
    "font_style",
    "size",
    "align",
    "tracking",
    "kerning",
    "ligatures",
    "leading",
    "baseline_shift",
    "faux_bold",
    "faux_italic",
    "caps",
    "underline",
    "box_width",
    "box_height",
    "vertical",
    // Shape geometry.
    "shape",
    "sides",
    "corner_radius",
    "points",
    // Transform.
    "position",
    "anchor",
    "scale",
    "scale_width",
    "uniform_scale",
    "rotation",
];

/// Fold `bytes` into the FNV-1a-style hash accumulator `h`, allowing callers to chain fields.
fn hash_bytes(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Hash the bits of a finite `f64`; all non-finite values share one representation so NaN
/// payloads or evaluation details cannot change a layout identity.
fn hash_f64(h: &mut u64, v: f64) {
    // Normalise non-finite values: NaN is "auto" for point parameters and must not make the
    // fingerprint depend on payload bits or an evaluation detail.
    if v.is_finite() {
        hash_bytes(h, &v.to_bits().to_le_bytes());
    } else {
        hash_bytes(h, b"non-finite");
    }
}

/// Hash an `f32` widened to `f64`, keeping the non-finite normalization of [`hash_f64`].
fn hash_f32(h: &mut u64, v: f32) {
    hash_f64(h, f64::from(v));
}

/// Hash an optional value with an explicit presence byte, followed by the inner value only
/// when it is present. This keeps `None` distinct from any hashed `Some` payload.
fn hash_option<T>(h: &mut u64, value: Option<T>, hash: impl FnOnce(&mut u64, T)) {
    match value {
        Some(v) => {
            hash_bytes(h, &[1]);
            hash(h, v);
        }
        None => hash_bytes(h, &[0]),
    }
}

/// Hash a layout parameter by kind and value. Non-finite floats normalize through
/// [`hash_f64`]; path payloads contribute only a tag because layout parameters do not use
/// mask paths.
fn hash_param_value(h: &mut u64, value: &ParamValue) {
    match value {
        ParamValue::Float(v) => {
            hash_bytes(h, b"f");
            hash_f64(h, *v);
        }
        ParamValue::Vec2(v) => {
            hash_bytes(h, b"v");
            hash_f64(h, v.x);
            hash_f64(h, v.y);
        }
        ParamValue::Color(c) => {
            hash_bytes(h, b"c");
            for v in c {
                hash_f32(h, *v);
            }
        }
        ParamValue::Bool(v) => {
            hash_bytes(h, b"b");
            hash_bytes(h, &[u8::from(*v)]);
        }
        ParamValue::Choice(v) => {
            hash_bytes(h, b"o");
            hash_bytes(h, &u64::from(*v).to_le_bytes());
        }
        ParamValue::Text(v) => {
            hash_bytes(h, b"t");
            hash_bytes(h, v.as_bytes());
        }
        ParamValue::Curve(points) => {
            hash_bytes(h, b"u");
            hash_bytes(h, &(points.len() as u64).to_le_bytes());
            for p in points {
                hash_f32(h, p[0]);
                hash_f32(h, p[1]);
            }
        }
        // Mask paths are not used by layer layout parameters.
        ParamValue::Path(_) => hash_bytes(h, b"p"),
    }
}

/// Fingerprint every layout-affecting value of `effect` at tick `t`: the layout parameters
/// evaluated at the playhead, character runs and pin data. Non-finite floats normalize, and
/// the committed text bytes are not hashed directly, so caret/selection changes do not churn
/// the fingerprint; a value or run change that can move the caret does.
fn layout_fingerprint(effect: &EffectInstance, t: Tick) -> u64 {
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
    hash_bytes(&mut h, effect.effect.as_bytes());
    hash_bytes(&mut h, &[u8::from(effect.enabled)]);
    for id in LAYOUT_PARAMS {
        let Some(param) = effect.params.get(*id) else { continue };
        hash_bytes(&mut h, id.as_bytes());
        hash_param_value(&mut h, &param.value_at(t));
    }
    if let Some(extra) = &effect.layer {
        hash_bytes(&mut h, b"runs");
        hash_bytes(&mut h, &(extra.runs.len() as u64).to_le_bytes());
        for run in &extra.runs {
            hash_bytes(&mut h, &(run.start as u64).to_le_bytes());
            hash_bytes(&mut h, &(run.end as u64).to_le_bytes());
            hash_option(&mut h, run.style.font.as_ref(), |h, v| hash_bytes(h, v.as_bytes()));
            hash_option(&mut h, run.style.font_style.as_ref(), |h, v| hash_bytes(h, v.as_bytes()));
            hash_option(&mut h, run.style.size, hash_f32);
            hash_option(&mut h, run.style.fill, |h, v| {
                for c in v {
                    hash_f32(h, c);
                }
            });
            hash_option(&mut h, run.style.faux_bold, |h, v| hash_bytes(h, &[u8::from(v)]));
            hash_option(&mut h, run.style.faux_italic, |h, v| hash_bytes(h, &[u8::from(v)]));
            hash_option(&mut h, run.style.underline, |h, v| hash_bytes(h, &[u8::from(v)]));
            hash_option(&mut h, run.style.tracking, hash_f32);
            hash_option(&mut h, run.style.baseline_shift, hash_f32);
            hash_option(&mut h, run.style.caps, |h, v| hash_bytes(h, &u64::from(v).to_le_bytes()));
        }
        hash_bytes(&mut h, b"pin");
        match &extra.pin {
            Some(pin) => {
                hash_bytes(&mut h, &[1]);
                match pin.to {
                    filmcraft_project::graphic_design::PinTarget::Frame => hash_bytes(&mut h, b"frame"),
                    filmcraft_project::graphic_design::PinTarget::Layer(uid) => {
                        hash_bytes(&mut h, b"layer");
                        hash_bytes(&mut h, &uid.to_le_bytes());
                    }
                }
                hash_bytes(&mut h, &[u8::from(pin.left), u8::from(pin.top), u8::from(pin.right), u8::from(pin.bottom)]);
                for offset in pin.offsets {
                    hash_f64(&mut h, offset);
                }
            }
            None => hash_bytes(&mut h, &[0]),
        }
    }
    h
}

/// Layout fingerprint of the layer at `layer` evaluated at the playhead's source time;
/// `None` when the sequence, clip or layer is missing.
pub(super) fn current_layer_layout_fingerprint(session: &Session, clip: u64, layer: usize) -> Option<u64> {
    let sequence = session.active_sequence()?;
    let t = session.playhead();
    let (_, item) = sequence.find_item(ClipId(clip))?;
    let &effect_index = layer_indices(&item.effects).get(layer)?;
    let effect = item.effects.get(effect_index)?;
    Some(layout_fingerprint(effect, item.source_time_at(t)))
}
