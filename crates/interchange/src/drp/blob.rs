//! The binary fields of a DaVinci Resolve project (`…BA`, `FieldsBlob`, `Clip`, `FrameRate`…),
//! as they appear hex-encoded in the XML of a `.drp`.
//!
//! What is known about them was read off projects Resolve 19–20 wrote (no Blackmagic software or
//! SDK was inspected):
//!
//! - **Framed blobs** start with a big-endian `u32` format (2) and a big-endian `u32` length, then
//!   one flag byte: `0x80` raw, `0x81` a Zstandard frame (RFC 8878). The payload is a Protocol
//!   Buffers message (the public wire format: varints, fixed 32/64 bit, length-delimited fields).
//! - **Effect filters** (`EffectFiltersBA`): repeated field 1, one message per effect: field 1 the
//!   effect id, repeated field 9 its parameters, each with field 1 the parameter id and the value
//!   a double at fields 3 → 1 → 2. The effects and parameters we map are listed in [`params`].
//! - **Rich text** of a Text generator: a run of `u32` little-endian lengths (counting themselves)
//!   followed by UTF-16LE strings: a layout string `b` + `a`×n + `` ` ``×n, the n text runs, then a
//!   font family, a style and a `#rrggbb` colour per run.
//! - **Media clip info** (`BtVideoInfo/Clip`, `BtAudioInfo/Clip`): field 1 the directory, field 2
//!   the file name.
//! - **Rates and extents**: little-endian doubles; resolutions two big-endian `u64`.

use std::collections::HashMap;
use std::io::Read as _;

/// Largest blob payload we decompress.
const MAX_BLOB: usize = 64 << 20;
/// Deepest protobuf nesting we walk.
const MAX_DEPTH: usize = 16;

/// Decode a hex string (whitespace ignored). `None` when it is not hex.
pub(crate) fn hex(s: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    let val = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    digits.as_chunks::<2>().0.iter().map(|[a, b]| Some(val(*a)? << 4 | val(*b)?)).collect()
}

fn f64_le(b: &[u8]) -> Option<f64> {
    let a: [u8; 8] = b.get(..8)?.try_into().ok()?;
    let v = f64::from_le_bytes(a);
    v.is_finite().then_some(v)
}

fn f64_be(b: &[u8]) -> Option<f64> {
    let a: [u8; 8] = b.get(..8)?.try_into().ok()?;
    let v = f64::from_be_bytes(a);
    v.is_finite().then_some(v)
}

/// A frame rate field (`FrameRate`, `MediaFrameRate`): frames per second, little-endian double.
pub(crate) fn frame_rate(hex_text: &str) -> Option<f64> {
    f64_le(&hex(hex_text)?).filter(|r| *r > 0.0 && *r <= 1000.0)
}

/// A resolution field: (width, height), two big-endian `u64`.
pub(crate) fn resolution(hex_text: &str) -> Option<(u32, u32)> {
    let b = hex(hex_text)?;
    let w = u64::from_be_bytes(b.get(..8)?.try_into().ok()?);
    let h = u64::from_be_bytes(b.get(8..16)?.try_into().ok()?);
    let ok = |v: u64| (16..=32_768).contains(&v);
    (ok(w) && ok(h)).then_some((w as u32, h as u32))
}

/// A sequence's `MediaExtents`: (start, duration) in seconds, little-endian doubles.
pub(crate) fn extents(hex_text: &str) -> Option<(f64, f64)> {
    let b = hex(hex_text)?;
    Some((f64_le(&b)?, f64_le(b.get(8..)?)?))
}

/// A clip's `MediaTimemapBA`: the media duration in seconds (a tag byte, then big-endian doubles).
pub(crate) fn timemap_duration(hex_text: &str) -> Option<f64> {
    let b = hex(hex_text)?;
    f64_be(b.get(1..)?).filter(|d| *d > 0.0 && *d < 1e7)
}

/// A time field (`Start`, `Duration`, `In`): whole frames, optionally `|` and a little-endian
/// double (hex) with the sub-frame fraction. Returns (frames, fraction in 0..1).
pub(crate) fn frames(s: &str) -> Option<(i64, f64)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (whole, frac) = match s.split_once('|') {
        Some((w, f)) => (w, hex(f).and_then(|b| f64_le(&b)).unwrap_or(0.0)),
        None => (s, 0.0),
    };
    let whole = whole.trim().parse::<i64>().ok()?;
    // frames beyond ~100 days at 1000 fps are not a time
    if whole.unsigned_abs() > 10_000_000_000 {
        return None;
    }
    Some((whole, if (0.0..1.0).contains(&frac) { frac } else { 0.0 }))
}

/// The payload of a framed blob: raw or Zstandard-decompressed.
pub(crate) fn payload(b: &[u8]) -> Option<Vec<u8>> {
    let format = u32::from_be_bytes(b.get(..4)?.try_into().ok()?);
    if format != 2 {
        return None;
    }
    let len = u32::from_be_bytes(b.get(4..8)?.try_into().ok()?) as usize;
    let body = b.get(8..8usize.checked_add(len)?)?;
    let (&flag, rest) = body.split_first()?;
    match flag {
        0x80 => Some(rest.to_vec()),
        0x81 => {
            let mut dec = ruzstd::decoding::StreamingDecoder::new(rest).ok()?;
            let mut out = Vec::new();
            (&mut dec).take(MAX_BLOB as u64 + 1).read_to_end(&mut out).ok()?;
            (out.len() <= MAX_BLOB).then_some(out)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Protocol Buffers wire format
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Field<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    Fixed32([u8; 4]),
    Bytes(&'a [u8]),
}

fn varint(b: &[u8], p: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let c = *b.get(*p)?;
        *p += 1;
        v |= u64::from(c & 0x7f) << shift;
        if c < 0x80 {
            return Some(v);
        }
    }
    None
}

/// The fields of a message, in order. `None` when the bytes are not a well-formed message.
pub(crate) fn fields(b: &[u8]) -> Option<Vec<(u64, Field<'_>)>> {
    let mut out = Vec::new();
    let mut p = 0;
    while p < b.len() {
        let key = varint(b, &mut p)?;
        let (num, wire) = (key >> 3, key & 7);
        if num == 0 {
            return None;
        }
        let f = match wire {
            0 => Field::Varint(varint(b, &mut p)?),
            1 => {
                let a: [u8; 8] = b.get(p..p.checked_add(8)?)?.try_into().ok()?;
                p += 8;
                Field::Fixed64(a)
            }
            2 => {
                let len = usize::try_from(varint(b, &mut p)?).ok()?;
                let s = b.get(p..p.checked_add(len)?)?;
                p += len;
                Field::Bytes(s)
            }
            5 => {
                let a: [u8; 4] = b.get(p..p.checked_add(4)?)?.try_into().ok()?;
                p += 4;
                Field::Fixed32(a)
            }
            _ => return None,
        };
        out.push((num, f));
    }
    Some(out)
}

fn sub<'a>(fs: &[(u64, Field<'a>)], num: u64) -> Option<&'a [u8]> {
    fs.iter().find_map(|(n, f)| match f {
        Field::Bytes(b) if *n == num => Some(*b),
        _ => None,
    })
}

fn varint_field(fs: &[(u64, Field<'_>)], num: u64) -> Option<u64> {
    fs.iter().find_map(|(n, f)| match f {
        Field::Varint(v) if *n == num => Some(*v),
        _ => None,
    })
}

/// A parameter's value: fields 3 → 1 → 2 (a double).
fn param_value(slot: &[(u64, Field<'_>)]) -> Option<f64> {
    let mut b = sub(slot, 3)?;
    for _ in 0..MAX_DEPTH {
        let fs = fields(b)?;
        if let Some(Field::Fixed64(a)) = fs.iter().find(|(n, _)| *n == 2).map(|(_, f)| *f) {
            let v = f64::from_le_bytes(a);
            return v.is_finite().then_some(v);
        }
        b = sub(&fs, 1)?;
    }
    None
}

/// Effect ids and their parameter ids, as Resolve numbers them in `EffectFiltersBA`.
pub(crate) mod params {
    /// Inspector ▸ Transform (video clips and generators).
    pub const TRANSFORM: u64 = 4;
    pub const POSITION_X: u64 = 40;
    pub const POSITION_Y: u64 = 41;
    pub const ZOOM_X: u64 = 42;
    pub const ZOOM_Y: u64 = 43;
    pub const ROTATION: u64 = 47;
    /// Video fade handles, in timeline frames.
    pub const VIDEO_FADE: u64 = 72;
    pub const VIDEO_FADE_IN: u64 = 137;
    pub const VIDEO_FADE_OUT: u64 = 138;
    /// Inspector ▸ Volume and the audio fade handles (frames).
    pub const AUDIO: u64 = 124;
    pub const AUDIO_VOLUME_DB: u64 = 95;
    pub const AUDIO_FADE_IN: u64 = 97;
    pub const AUDIO_FADE_OUT: u64 = 98;
}

/// Effect id → (parameter id → value) of an `EffectFiltersBA` blob.
pub(crate) fn effect_params(hex_text: &str) -> HashMap<u64, HashMap<u64, f64>> {
    let mut out: HashMap<u64, HashMap<u64, f64>> = HashMap::new();
    let Some(data) = hex(hex_text).and_then(|b| payload(&b)) else { return out };
    let Some(top) = fields(&data) else { return out };
    for (n, f) in &top {
        let (1, Field::Bytes(eb)) = (*n, f) else { continue };
        let Some(effect) = fields(eb) else { continue };
        let Some(id) = varint_field(&effect, 1) else { continue };
        let entry = out.entry(id).or_default();
        for (sn, sf) in &effect {
            let (9, Field::Bytes(sb)) = (*sn, sf) else { continue };
            let Some(slot) = fields(sb) else { continue };
            if let (Some(pid), Some(v)) = (varint_field(&slot, 1), param_value(&slot)) {
                entry.insert(pid, v);
            }
        }
    }
    out
}

/// The media file of a `BtVideoInfo` / `BtAudioInfo` `Clip` blob: directory (field 1) joined with
/// the file name (field 2).
pub(crate) fn clip_path(hex_text: &str) -> Option<String> {
    let data = hex(hex_text).and_then(|b| payload(&b))?;
    let fs = fields(&data)?;
    let utf8 = |n| sub(&fs, n).and_then(|b| std::str::from_utf8(b).ok()).map(str::trim).filter(|s| !s.is_empty());
    let dir = utf8(1)?;
    let name = utf8(2)?;
    if !(dir.starts_with('/') || dir.starts_with("\\\\") || dir.get(1..3).is_some_and(|d| d == ":\\" || d == ":/")) {
        return None;
    }
    let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
    Some(format!("{}{sep}{name}", dir.trim_end_matches(['/', '\\'])))
}

/// The text of a title generator.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TitleText {
    pub text: String,
    pub font: Option<String>,
    pub style: Option<String>,
    /// RGBA, 0..1.
    pub color: Option<[f32; 4]>,
    /// Font size as a fraction of the frame width (Fusion Text+), when known.
    pub size: Option<f64>,
}

fn utf16_strings(d: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 6 <= d.len() && out.len() < 10_000 {
        let n = d.get(p..p + 4).and_then(|s| s.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0) as usize;
        if (6..=0x10000).contains(&n) && n.is_multiple_of(2) && p + n <= d.len() {
            let units: Vec<u16> = d[p + 4..p + n].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            if let Ok(s) = String::from_utf16(&units)
                && !s.is_empty()
                && s.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}'))
            {
                out.push(s);
                p += n;
                continue;
            }
        }
        p += 1;
    }
    out
}

fn css_color(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|v| f32::from(v) / 255.0);
    Some([c(0)?, c(2)?, c(4)?, 1.0])
}

/// The text of a title generator's `EffectFiltersBA` (rich text runs, the first run's font, style
/// and colour). `None` when the blob holds no text.
pub(crate) fn title_text(hex_text: &str) -> Option<TitleText> {
    let data = hex(hex_text).and_then(|b| payload(&b))?;
    let strings = utf16_strings(&data);
    let layout = strings.iter().position(|s| {
        let a = s.strip_prefix('b').unwrap_or("");
        let runs = a.chars().take_while(|c| *c == 'a').count();
        runs > 0 && a.len() == runs * 2 && a[runs..].chars().all(|c| c == '`')
    })?;
    let runs = strings.get(layout)?.chars().filter(|c| *c == 'a').count();
    let texts = strings.get(layout + 1..layout + 1 + runs)?;
    // Runs ending in one space continue the line; any other run ends it.
    let mut text = String::new();
    for (i, r) in texts.iter().enumerate() {
        let joins = r.ends_with(' ') && !r.ends_with("  ");
        text.push_str(if joins { r } else { r.trim_end() });
        if !joins && i + 1 < texts.len() {
            text.push('\n');
        }
    }
    let text = text.replace(['\u{2028}', '\u{2029}', '\r'], "\n").trim().to_string();
    if text.is_empty() {
        return None;
    }
    let rest = strings.get(layout + 1 + runs..).unwrap_or(&[]);
    let font = rest.first().filter(|s| css_color(s).is_none()).cloned();
    let style = rest.get(1).filter(|s| css_color(s).is_none()).cloned();
    let color = rest.iter().take(4).find_map(|s| css_color(s));
    Some(TitleText { text, font, style, color, size: None })
}

/// The UTF-8 strings of a framed protobuf blob (`FieldsBlob`), depth first. A multicam clip
/// names its active angle here (`Camera 2`).
pub(crate) fn strings(hex_text: &str) -> Vec<String> {
    fn walk(b: &[u8], depth: usize, out: &mut Vec<String>) {
        let Some(fs) = fields(b) else { return };
        for (_, f) in fs {
            let Field::Bytes(x) = f else { continue };
            if let Ok(s) = std::str::from_utf8(x)
                && !s.is_empty()
                && s.len() <= 512
                && s.chars().all(|c| !c.is_control())
            {
                out.push(s.to_string());
            }
            if depth < MAX_DEPTH && out.len() < 1000 {
                walk(x, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(d) = hex(hex_text).and_then(|b| payload(&b)) {
        walk(&d, 0, &mut out);
    }
    out
}

/// Qt's `qCompress` framing: a big-endian `u32` length, then a zlib stream.
fn q_uncompress(b: &[u8]) -> Option<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(b.get(4..)?, MAX_BLOB).ok()
}

/// The tools of a Fusion composition (`CompositionBA`): the composition is `qCompress`ed and
/// its tools, in Fusion's text format, are a zlib stream after `Compressed = true`.
fn fusion_tools(hex_text: &str) -> Option<String> {
    let outer = q_uncompress(&hex(hex_text)?)?;
    let marker = b"Compressed = true";
    let Some(at) = outer.windows(marker.len()).position(|w| w == marker) else {
        return Some(String::from_utf8_lossy(&outer).into_owned());
    };
    let rest = outer.get(at..)?;
    (0..rest.len().saturating_sub(1).min(64)).find_map(|i| {
        let (a, b) = (*rest.get(i)?, *rest.get(i + 1)?);
        // a zlib header: CM 8, and the FCHECK bits make it a multiple of 31
        if a & 0x0f != 8 || !(u16::from(a) << 8 | u16::from(b)).is_multiple_of(31) {
            return None;
        }
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(rest.get(i..)?, MAX_BLOB).ok().map(|t| String::from_utf8_lossy(&t).into_owned())
    })
}

/// The value of `Name = Input { Value = … }` in Fusion's text format: a quoted string
/// (unescaped) or a bare number.
fn fusion_input(tools: &str, name: &str) -> Option<String> {
    let key = format!("{name} = Input {{");
    let at = tools.find(&key)? + key.len();
    let rest = tools.get(at..)?;
    let v = rest.find("Value = ")? + "Value = ".len();
    if rest.get(..v)?.contains('}') {
        return None;
    }
    let rest = rest.get(v..)?;
    if let Some(q) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = q.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    c => out.push(c),
                },
                c => out.push(c),
            }
        }
        None
    } else {
        Some(rest.split([',', ' ', '}', '\n']).next()?.trim().to_string())
    }
}

/// The text of a Fusion title (Text+, or a title template built on it) from its composition.
pub(crate) fn fusion_title(hex_text: &str) -> Option<TitleText> {
    let tools = fusion_tools(hex_text)?;
    let text = fusion_input(&tools, "StyledText")?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let num = |n: &str| fusion_input(&tools, n).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
    let color = match (num("Red1"), num("Green1"), num("Blue1")) {
        (None, None, None) => None,
        (r, g, b) => Some([r.unwrap_or(1.0), g.unwrap_or(1.0), b.unwrap_or(1.0), 1.0].map(|c| c.clamp(0.0, 1.0) as f32)),
    };
    Some(TitleText {
        text,
        font: fusion_input(&tools, "Font").filter(|s| !s.is_empty()),
        style: fusion_input(&tools, "Style").filter(|s| !s.is_empty()),
        color,
        size: num("Size").filter(|s| *s > 0.0 && *s < 10.0),
    })
}

#[cfg(test)]
pub(crate) mod build {
    //! Writers for the blob formats above (tests only).

    pub fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    pub fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let c = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(c);
                return;
            }
            out.push(c | 0x80);
        }
    }

    pub fn bytes_field(num: u64, b: &[u8], out: &mut Vec<u8>) {
        varint(num << 3 | 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }

    /// A raw (`0x80`) framed blob around `payload`.
    pub fn framed(payload: &[u8]) -> Vec<u8> {
        let mut out = 2u32.to_be_bytes().to_vec();
        out.extend_from_slice(&((payload.len() + 1) as u32).to_be_bytes());
        out.push(0x80);
        out.extend_from_slice(payload);
        out
    }

    /// `EffectFiltersBA` with `(effect id, [(param id, value)])`.
    pub fn effects(list: &[(u64, &[(u64, f64)])]) -> String {
        let mut top = Vec::new();
        for (id, ps) in list {
            let mut e = Vec::new();
            varint(1 << 3, &mut e);
            varint(*id, &mut e);
            for (pid, v) in *ps {
                let mut val = Vec::new();
                varint(2 << 3 | 1, &mut val);
                val.extend_from_slice(&v.to_le_bytes());
                let mut v1 = Vec::new();
                bytes_field(1, &val, &mut v1);
                let mut slot = Vec::new();
                varint(1 << 3, &mut slot);
                varint(*pid, &mut slot);
                bytes_field(3, &v1, &mut slot);
                bytes_field(9, &slot, &mut e);
            }
            bytes_field(1, &e, &mut top);
        }
        hex(&framed(&top))
    }

    fn utf16(s: &str, out: &mut Vec<u8>) {
        let units: Vec<u16> = s.encode_utf16().collect();
        out.extend_from_slice(&((units.len() * 2 + 4) as u32).to_le_bytes());
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }

    /// A title generator's `EffectFiltersBA` with rich-text `runs` in `font` / `style` / `color`.
    pub fn title(runs: &[&str], font: &str, style: &str, color: &str) -> String {
        let mut d = vec![0x0a, 0x01, 0x00];
        utf16(&format!("b{}{}", "a".repeat(runs.len()), "`".repeat(runs.len())), &mut d);
        d.extend_from_slice(&[0x0f, 0, 0, 0, 0x08, 0, 0, 0]);
        for r in runs {
            utf16(r, &mut d);
            d.extend_from_slice(&[0x02, 0, 0, 0, 0x01, 0x00]);
        }
        for _ in runs {
            utf16(font, &mut d);
            d.extend_from_slice(&[0, 0, 0xc0, 0x42]);
            utf16(style, &mut d);
            d.push(0x04);
            utf16(color, &mut d);
        }
        hex(&framed(&d))
    }

    /// A `BtVideoInfo/Clip` blob for `dir` / `name`.
    pub fn clip(dir: &str, name: &str) -> String {
        let mut p = Vec::new();
        bytes_field(1, dir.as_bytes(), &mut p);
        bytes_field(2, name.as_bytes(), &mut p);
        hex(&framed(&p))
    }

    /// A `CompositionBA` holding a Text+ tool with `text` (quoted and escaped) in `font`.
    pub fn fusion(text: &str, font: &str) -> String {
        let esc = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
        let tools = format!(
            "{{ Template = TextPlus {{ Inputs = {{ StyledText = Input {{ Value = \"{esc}\", }}, Font = Input {{ Value = \"{font}\", }}, Size = Input {{ Value = 0.09, }}, Red1 = Input {{ Value = 0, }} }} }} }}"
        );
        let inner = miniz_oxide::deflate::compress_to_vec_zlib(tools.as_bytes(), 6);
        let mut comp = b"Composition { CustomData = { TEMPLATE_ID = \"Text+\" }, Compressed = true, }\x00".to_vec();
        comp.extend_from_slice(&(tools.len() as u32).to_le_bytes());
        comp.extend_from_slice(&inner);
        let mut out = (comp.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(&comp, 6));
        hex(&out)
    }

    /// A `FieldsBlob` naming the active multicam angle.
    pub fn angle(name: &str) -> String {
        let mut inner = Vec::new();
        bytes_field(3, name.as_bytes(), &mut inner);
        let mut p = Vec::new();
        bytes_field(1, &inner, &mut p);
        hex(&framed(&p))
    }

    /// A little-endian double as hex (rates, extents, sub-frame fractions).
    pub fn f64_le(v: f64) -> String {
        hex(&v.to_le_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(frame_rate("0000000000003e400000000000000000"), Some(30.0));
        assert_eq!(frame_rate(&format!("{}0000000000000000", build::f64_le(24000.0 / 1001.0))).map(|r| (r * 1000.0).round()), Some(23976.0));
        assert_eq!(resolution("0000000000000f000000000000000870"), Some((3840, 2160)));
        assert_eq!(resolution("00000000000000000000000000000000"), None);
        assert_eq!(extents("000000000020ac404044444444b46b40").map(|e| e.0), Some(3600.0));
        assert_eq!(timemap_duration("02406FE033E1F67153").map(|d| d.round()), Some(255.0));
        assert_eq!(frames("108000"), Some((108000, 0.0)));
        assert_eq!(frames("64|00a8f1d24d62b03f").map(|(f, x)| (f, (x * 1000.0).round())), Some((64, 64.0)));
        assert_eq!(frames(""), None);
        assert_eq!(frames("abc"), None);
        assert_eq!(frames("99999999999999999"), None);
    }

    #[test]
    fn real_blobs() {
        // From a Resolve 20 project: a raw transform blob and a Zstandard-compressed clip info.
        let e = effect_params("000000020000001d800a1a08481806380c4a004a10088a011a0b0a09110000000000405140");
        assert_eq!(e.get(&params::VIDEO_FADE).and_then(|p| p.get(&params::VIDEO_FADE_OUT)), Some(&69.0));
        assert!(effect_params("").is_empty());
        assert!(effect_params("zz").is_empty());
    }

    #[test]
    fn built_blobs() {
        let e = effect_params(&build::effects(&[(params::AUDIO, &[(params::AUDIO_VOLUME_DB, -6.5), (params::AUDIO_FADE_IN, 12.0)])]));
        assert_eq!(e[&params::AUDIO][&params::AUDIO_VOLUME_DB], -6.5);
        assert_eq!(e[&params::AUDIO][&params::AUDIO_FADE_IN], 12.0);
        assert_eq!(clip_path(&build::clip("/Volumes/Data/USA Files/", "P1.MP4")).as_deref(), Some("/Volumes/Data/USA Files/P1.MP4"));
        assert_eq!(clip_path(&build::clip("C:\\Media", "a.mov")).as_deref(), Some("C:\\Media\\a.mov"));
        assert_eq!(clip_path(&build::clip("relative", "a.mov")), None);
        let t = title_text(&build::title(&["USA ", "2025"], "Bebas Neue", "Regular", "#00ffff")).unwrap();
        assert_eq!(t.text, "USA 2025");
        assert_eq!(t.font.as_deref(), Some("Bebas Neue"));
        assert_eq!(t.style.as_deref(), Some("Regular"));
        assert_eq!(t.color, Some([0.0, 1.0, 1.0, 1.0]));
        let t = title_text(&build::title(&["Credits  ", "Nik", "Camera  "], "Open Sans", "Semibold", "#ffffff")).unwrap();
        assert_eq!(t.text, "Credits\nNik\nCamera");
        assert_eq!(title_text(&build::title(&["  "], "Open Sans", "Semibold", "#ffffff")), None);
        let f = fusion_title(&build::fusion("Sequoia \"West\"\nUSA", "Brush Script MT")).unwrap();
        assert_eq!(f.text, "Sequoia \"West\"\nUSA");
        assert_eq!(f.font.as_deref(), Some("Brush Script MT"));
        assert_eq!(f.size, Some(0.09));
        assert_eq!(f.color, Some([0.0, 1.0, 1.0, 1.0]));
        assert!(strings(&build::angle("Camera 2")).iter().any(|s| s == "Camera 2"));
    }

    #[test]
    fn hostile_blobs_never_panic() {
        let seeds = [
            build::effects(&[(params::TRANSFORM, &[(params::ZOOM_X, 1.5), (params::POSITION_X, 0.25)])]),
            build::title(&["A ", "B"], "F", "Regular", "#ffffff"),
            build::clip("/a", "b.mov"),
            build::fusion("Hello", "Open Sans"),
            build::angle("Camera 1"),
        ];
        for seed in seeds {
            let b = hex(&seed).unwrap();
            for cut in 0..b.len() {
                let h = build::hex(&b[..cut]);
                let _ = (effect_params(&h), title_text(&h), clip_path(&h), fusion_title(&h), strings(&h));
            }
            for i in 0..b.len() {
                for bit in [0x01u8, 0x80, 0xff] {
                    let mut m = b.clone();
                    m[i] ^= bit;
                    let h = build::hex(&m);
                    let _ = (effect_params(&h), title_text(&h), clip_path(&h), fusion_title(&h), strings(&h));
                }
            }
        }
    }
}
