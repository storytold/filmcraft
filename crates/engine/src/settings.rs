//! **Settings** (Premiere: app menu ▸ Settings ▸ <category>; the window is titled "Preferences").
//!
//! The values live in [`crate::autosave::Preferences`] (persisted as `preferences.json` in the
//! per-user data directory, never in the project). Every value is a dotted camelCase key whose first
//! segment is the category id (`timeline.stillImageDuration`, `labels.colors.violet.name`), so the
//! `prefs.get` / `prefs.set` / `prefs.reset` commands reach all of them from the CLI, the control
//! channel and MCP, and the Settings dialog's controls carry the automation id `settings.<key>`.
//!
//! This module holds the categories' value types, the field schema the dialog is drawn from
//! ([`categories`]; also returned by `prefs.schema` so agents can discover choices and ranges),
//! validation/migration, and the engine-side helpers that make the values take effect (default
//! durations, label colours, the media cache, audio output mapping, smart quotes…).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use filmcraft_project::Label;
use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{EngineError, Result, Session};

mod commands;
mod labels;
mod media_cache;
mod prefs;
mod schema;

pub use commands::commands;
pub use labels::{
    DEFAULT_LABEL_PRESET, LABEL_PRESETS_DIR, LabelColor, LabelDefaults, LabelPrefs, LabelPreset, PresetLabel, hex, is_builtin_label_preset, label_id, parse_hex,
};
pub use media_cache::{cache_usage, clean, enforce_policy, media_cache_dir, previews_root};
pub use prefs::{
    AppearancePrefs, AudioHardwarePrefs, ColorPrefs, DEFAULT_HIGHLIGHT, GeneralPrefs, GraphicsPrefs, MediaAnalysisPrefs, MediaCachePrefs, MemoryPrefs,
    PluginPrefs, TimelinePrefs, timebase_rate,
};
pub use schema::{
    APPEARANCE_MODES, Category, DARK_THEMES, DURATION_UNITS, DeviceList, Field, Kind, LIGHT_THEMES, Row, UI_SCALES, categories, category, field, fields,
    is_unit_key, migrate, sanitize, validate,
};

/// Version of the preferences file layout ([`crate::autosave::Preferences::version`]).
/// v1: Auto Save, Audio (mixer automation), Playback (pre/postroll), Trim, Media (proxies), guides,
/// Essential Sound presets. v2: every Settings category.
pub const PREFS_VERSION: u32 = 2;
const SPEAKERS: &[(&str, &str)] = &[("on", "Label speakers"), ("off", "Don't label speakers")];
const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("ru", "Russian"),
    ("hi", "Hindi"),
];
const CACHE_MGMT: &[(&str, &str)] = &[
    ("never", "Do not delete cache files automatically"),
    ("olderThan", "Automatically delete cache files older than"),
    ("exceedsSize", "Automatically delete oldest cache files when cache exceeds"),
];

// ------------------------------------------------------------------ helpers used by the engine

/// Typographer's quotes: `"a 'b'"` → `“a ‘b’”`, apostrophes inside words → `’`.
pub fn smart_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        let opening = prev.is_none_or(|p| p.is_whitespace() || "([{-–—“‘".contains(p));
        out.push(match c {
            '"' if opening => '“',
            '"' => '”',
            '\'' if opening => '‘',
            '\'' => '’',
            c => c,
        });
        prev = Some(c);
    }
    out
}

/// Interleave a stereo mix into a device buffer of `ch` channels following Output Mapping: left
/// to device channel `map[0]`, right to `map[1]` (others silent). A mono device gets the sum.
/// Out-of-range mappings fall back to channels 0/1.
pub fn map_output(left: &[f32], right: &[f32], out: &mut [f32], ch: usize, map: [u32; 2]) {
    let ch = ch.max(1);
    let n = (out.len() / ch).min(left.len()).min(right.len());
    out.iter_mut().for_each(|s| *s = 0.0);
    if ch == 1 {
        for i in 0..n {
            out[i] = (0.5 * (left[i] + right[i])).clamp(-1.0, 1.0);
        }
        return;
    }
    let pick = |m: u32, d: usize| if (m as usize) < ch { m as usize } else { d };
    let (l, r) = (pick(map[0], 0), pick(map[1], 1));
    for i in 0..n {
        out[i * ch + l] += left[i];
        out[i * ch + r] += right[i];
        out[i * ch + l] = out[i * ch + l].clamp(-1.0, 1.0);
        out[i * ch + r] = out[i * ch + r].clamp(-1.0, 1.0);
    }
}

/// Settings ▸ Media ▸ Default Media Scaling for a clip placed in a `frame`-sized sequence. `src`
/// is the clip's size at its display aspect in sequence pixels
/// ([`filmcraft_project::conformed_size`]): `scaleToFrameSize` turns on Scale to Frame Size
/// (rasterised at frame size), `setToFrameSize` sets Motion ▸ Scale so the picture fits the frame.
pub fn apply_media_scaling(ti: &mut filmcraft_project::TrackItem, scaling: &str, frame: (u32, u32), src: (f64, f64)) {
    if !(src.0 >= 1e-9 && src.1 >= 1e-9 && src.0.is_finite() && src.1.is_finite()) {
        return;
    }
    match scaling {
        "scaleToFrameSize" => ti.scale_to_frame = true,
        "setToFrameSize" => {
            let fit = (frame.0 as f64 / src.0).min(frame.1 as f64 / src.1);
            if let Some(m) = ti.effects.iter_mut().find(|e| e.effect == "motion") {
                m.params.insert("scale".into(), filmcraft_project::Param::new(filmcraft_project::ParamValue::Float((fit * 1000.0).round() / 10.0)));
            }
        }
        _ => {}
    }
}
