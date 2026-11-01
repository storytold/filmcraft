//! Essential Sound: clip audio types (Dialogue / Music / SFX / Ambience), their section settings
//! and presets, and how those settings become ordinary clip effects.
//!
//! The panel never processes audio itself. Like Premiere, it drives effects on the clip "under the
//! hood": Reduce Rumble is a Highpass, Reduce Noise a DeNoise, DeHum a DeHummer, DeEss a DeEsser,
//! Reduce Reverb a DeReverb, Dynamics a Dynamics Processing, EQ a Parametric Equalizer, Enhance
//! Speech the Enhance Speech chain, Stereo Width the Stereo Width effect and Reverb a Studio
//! Reverb. These instances carry [`EffectInstance::essential`], appear in Effect Controls (where
//! their parameters can be keyframed) and render through the normal clip-effect path, so the
//! mixer, playback and export need nothing special. Loudness auto-match writes clip gain, Clip
//! Volume the intrinsic Volume level, Pan the Panner, and Ducking writes Volume keyframes.
//!
//! [`apply`] reconciles a clip's essential effects with its settings after every change: missing
//! effects are inserted in canonical order, effects of slots that were switched off are removed,
//! and only parameters whose derived value changed are written (at the playhead, so keyframes the
//! user added in Effect Controls survive unrelated slider moves).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::effect::{EffectInstance, find_effect};
use crate::keyframe::ParamValue;
use crate::{Param, TrackItem};
use filmcraft_time::Tick;

/// Clip audio type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioType {
    Dialogue,
    Music,
    #[serde(rename = "SFX")]
    Sfx,
    Ambience,
}

impl AudioType {
    pub const ALL: [AudioType; 4] = [AudioType::Dialogue, AudioType::Music, AudioType::Sfx, AudioType::Ambience];
    pub fn label(self) -> &'static str {
        match self {
            AudioType::Dialogue => "Dialogue",
            AudioType::Music => "Music",
            AudioType::Sfx => "SFX",
            AudioType::Ambience => "Ambience",
        }
    }
    /// Command-parameter id (`dialogue`, `music`, `sfx`, `ambience`).
    pub fn id(self) -> &'static str {
        match self {
            AudioType::Dialogue => "dialogue",
            AudioType::Music => "music",
            AudioType::Sfx => "sfx",
            AudioType::Ambience => "ambience",
        }
    }
    pub fn parse(s: &str) -> Option<AudioType> {
        AudioType::ALL.into_iter().find(|t| t.id().eq_ignore_ascii_case(s) || t.label().eq_ignore_ascii_case(s))
    }
    /// The sections the panel shows for this type (Premiere's layout).
    pub fn sections(self) -> &'static [Section] {
        match self {
            AudioType::Dialogue => &[Section::Loudness, Section::Repair, Section::Clarity, Section::Creative],
            AudioType::Music => &[Section::Loudness, Section::Duration, Section::Ducking],
            AudioType::Sfx => &[Section::Loudness, Section::Creative, Section::Pan],
            AudioType::Ambience => &[Section::Loudness, Section::Creative, Section::Ducking],
        }
    }
    pub fn has(self, s: Section) -> bool {
        self.sections().contains(&s)
    }
}

/// A collapsible panel section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Section {
    Loudness,
    Repair,
    Clarity,
    Creative,
    Ducking,
    Duration,
    Pan,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Loudness => "Loudness",
            Section::Repair => "Repair",
            Section::Clarity => "Clarity",
            Section::Creative => "Creative",
            Section::Ducking => "Ducking",
            Section::Duration => "Duration",
            Section::Pan => "Pan",
        }
    }
    /// Settings key of the section's on/off switch (`None` for Duration, which has none).
    pub fn key(self) -> Option<&'static str> {
        Some(match self {
            Section::Loudness => "loudness.enabled",
            Section::Repair => "repair.enabled",
            Section::Clarity => "clarity.enabled",
            Section::Creative => "creative.enabled",
            Section::Ducking => "ducking.enabled",
            Section::Pan => "pan.enabled",
            Section::Duration => return None,
        })
    }
}

/// A checkbox + 0…10 slider pair.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub on: bool,
    pub amount: f64,
}

impl Slot {
    const fn off(amount: f64) -> Self {
        Slot { on: false, amount }
    }
    const fn at(amount: f64) -> Self {
        Slot { on: true, amount }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Loudness {
    pub enabled: bool,
    /// Integrated loudness measured by the last Auto-Match (LUFS, before the match gain).
    pub measured_lufs: Option<f64>,
    /// Target used by the last Auto-Match.
    pub target_lufs: Option<f64>,
    /// Gain the match applied (dB); part of the clip gain while the section is on.
    pub gain_db: f64,
}

impl Default for Loudness {
    fn default() -> Self {
        Loudness { enabled: true, measured_lufs: None, target_lufs: None, gain_db: 0.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Repair {
    pub enabled: bool,
    pub noise: Slot,
    pub rumble: Slot,
    pub dehum: Slot,
    /// Mains frequency for DeHum: 50 or 60 Hz.
    pub hum_hz: u32,
    pub deess: Slot,
    pub reverb: Slot,
}

impl Default for Repair {
    fn default() -> Self {
        Repair {
            enabled: true,
            noise: Slot::off(5.0),
            rumble: Slot::off(5.0),
            dehum: Slot::off(5.0),
            hum_hz: 60,
            deess: Slot::off(5.0),
            reverb: Slot::off(5.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clarity {
    pub enabled: bool,
    pub dynamics: Slot,
    pub eq: Slot,
    pub eq_preset: String,
    /// Enhance Speech; `amount` is the mix (0…10).
    pub enhance: Slot,
    /// 0 = low tone (lower voices), 1 = high tone.
    pub enhance_tone: u32,
}

impl Default for Clarity {
    fn default() -> Self {
        Clarity { enabled: true, dynamics: Slot::off(5.0), eq: Slot::off(5.0), eq_preset: EQ_PRESETS[0].name.into(), enhance: Slot::off(10.0), enhance_tone: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Creative {
    pub enabled: bool,
    pub reverb: Slot,
    pub reverb_preset: String,
    /// Stereo Width (Ambience): 0…10 → 100…200 %.
    pub width: Slot,
}

impl Default for Creative {
    fn default() -> Self {
        Creative { enabled: true, reverb: Slot::off(5.0), reverb_preset: REVERB_PRESETS[0].name.into(), width: Slot::off(5.0) }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ducking {
    pub enabled: bool,
    /// Duck against clips of these types.
    pub against: Vec<AudioType>,
    /// Also duck against clips without an audio type.
    pub against_untyped: bool,
    /// 0…10: how quiet a trigger may be and still count.
    pub sensitivity: f64,
    /// Reduction in dB (negative).
    pub reduce_db: f64,
    /// Fade length (s).
    pub fade_s: f64,
}

impl Default for Ducking {
    fn default() -> Self {
        Ducking { enabled: false, against: vec![AudioType::Dialogue], against_untyped: false, sensitivity: 6.0, reduce_db: -15.0, fade_s: 0.8 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pan {
    pub enabled: bool,
    /// −100 (left) … +100 (right).
    pub value: f64,
}

/// Clip Volume (footer): an offset in dB on the clip's Volume level while `on`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipVolume {
    pub on: bool,
    pub level_db: f64,
}

/// A clip's Essential Sound settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EssentialSound {
    pub kind: AudioType,
    /// Last applied preset ("(Default)" or a preset name); cleared when a setting is changed by hand.
    #[serde(default)]
    pub preset: String,
    #[serde(default)]
    pub loudness: Loudness,
    #[serde(default)]
    pub repair: Repair,
    #[serde(default)]
    pub clarity: Clarity,
    #[serde(default)]
    pub creative: Creative,
    #[serde(default)]
    pub ducking: Ducking,
    #[serde(default)]
    pub pan: Pan,
    #[serde(default)]
    pub volume: ClipVolume,
    #[serde(default)]
    pub mute: bool,
}

pub const DEFAULT_PRESET: &str = "(Default)";

impl EssentialSound {
    /// The type's default settings.
    pub fn new(kind: AudioType) -> Self {
        let mut ducking = Ducking::default();
        if kind == AudioType::Ambience {
            ducking.reduce_db = -10.0;
        }
        EssentialSound {
            kind,
            preset: DEFAULT_PRESET.into(),
            loudness: Loudness::default(),
            repair: Repair::default(),
            clarity: Clarity::default(),
            creative: Creative::default(),
            ducking,
            pan: Pan::default(),
            volume: ClipVolume::default(),
            mute: false,
        }
    }

    /// The settings as JSON (keys as in [`EssentialSound::set`]).
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// A setting by dotted key (`repair.noise.amount`, `clarity.eqPreset`…; snake_case is
    /// accepted too).
    pub fn get(&self, key: &str) -> Option<Value> {
        let v = self.to_value();
        let mut cur = &v;
        for part in key.split('.') {
            cur = cur.get(snake(part).as_str())?;
        }
        Some(cur.clone())
    }

    /// Set one setting by dotted key. Values are validated and clamped; the type cannot be changed
    /// here (use a type assignment, which resets the settings).
    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), String> {
        let parts: Vec<String> = key.split('.').map(snake).collect();
        if parts.first().map(String::as_str) == Some("kind") {
            return Err("the audio type is changed with essentialSound.setType".into());
        }
        let mut v = self.to_value();
        {
            let mut cur = &mut v;
            for part in &parts {
                cur = cur.get_mut(part.as_str()).ok_or_else(|| format!("unknown Essential Sound setting `{key}`"))?;
            }
            let ok = match (&*cur, value) {
                (Value::Bool(_), Value::Bool(_)) | (Value::String(_), Value::String(_)) | (Value::Array(_), Value::Array(_)) => true,
                (Value::Number(_), Value::Number(_)) | (Value::Null, Value::Number(_)) => true,
                (Value::Null, Value::Null) => true,
                (Value::Number(_), Value::Null) => false,
                _ => false,
            };
            if !ok {
                return Err(format!("`{key}`: expected {}", type_name(cur)));
            }
            *cur = value.clone();
        }
        let mut next: EssentialSound = serde_json::from_value(v).map_err(|e| format!("`{key}`: {e}"))?;
        next.clamp();
        if parts.first().map(String::as_str) == Some("clarity")
            && parts.get(1).map(String::as_str) == Some("eq_preset")
            && eq_preset(&next.clarity.eq_preset).is_none()
        {
            return Err(format!("unknown EQ preset `{}`", next.clarity.eq_preset));
        }
        if parts.first().map(String::as_str) == Some("creative")
            && parts.get(1).map(String::as_str) == Some("reverb_preset")
            && reverb_preset(&next.creative.reverb_preset).is_none()
        {
            return Err(format!("unknown reverb preset `{}`", next.creative.reverb_preset));
        }
        next.kind = self.kind;
        if !matches!(parts.first().map(String::as_str), Some("preset" | "volume" | "mute")) {
            next.preset.clear();
        }
        *self = next;
        Ok(())
    }

    fn clamp(&mut self) {
        let c = |v: &mut f64, lo: f64, hi: f64, def: f64| *v = if v.is_finite() { v.clamp(lo, hi) } else { def };
        for s in [
            &mut self.repair.noise,
            &mut self.repair.rumble,
            &mut self.repair.dehum,
            &mut self.repair.deess,
            &mut self.repair.reverb,
            &mut self.clarity.dynamics,
            &mut self.clarity.eq,
            &mut self.clarity.enhance,
            &mut self.creative.reverb,
            &mut self.creative.width,
        ] {
            c(&mut s.amount, 0.0, 10.0, 5.0);
        }
        self.repair.hum_hz = if self.repair.hum_hz < 55 { 50 } else { 60 };
        self.clarity.enhance_tone = self.clarity.enhance_tone.min(1);
        c(&mut self.ducking.sensitivity, 0.0, 10.0, 6.0);
        c(&mut self.ducking.reduce_db, -40.0, 0.0, -15.0);
        c(&mut self.ducking.fade_s, 0.0, 5.0, 0.8);
        self.ducking.against.dedup();
        c(&mut self.pan.value, -100.0, 100.0, 0.0);
        c(&mut self.volume.level_db, -96.0, 15.0, 0.0);
        c(&mut self.loudness.gain_db, -96.0, 96.0, 0.0);
    }

    /// Clip gain this state contributes (the Auto-Match gain while Loudness is on).
    pub fn loudness_gain_db(&self) -> f64 {
        if self.loudness.enabled { self.loudness.gain_db } else { 0.0 }
    }
    /// Offset this state adds to the Volume level.
    pub fn volume_offset_db(&self) -> f64 {
        if self.volume.on { self.volume.level_db } else { 0.0 }
    }
    /// Panner balance this state sets (`None` when Pan is not in use).
    pub fn pan_value(&self) -> Option<f64> {
        self.kind.has(Section::Pan).then_some(if self.pan.enabled { self.pan.value } else { 0.0 })
    }

    /// The clip effects these settings call for, in processing order, with their parameter values.
    pub fn effects(&self) -> Vec<EffectInstance> {
        let mut out = Vec::new();
        let k = self.kind;
        let mut push = |id: &str, section_on: bool, params: &[(&str, ParamValue)]| {
            if let Some(d) = find_effect(id) {
                let mut e = d.instance();
                e.essential = true;
                e.enabled = section_on;
                for (pid, v) in params {
                    if let Some(p) = e.params.get_mut(*pid) {
                        p.value = v.clone();
                    }
                }
                out.push(e);
            }
        };
        let f = ParamValue::Float;
        if k.has(Section::Repair) {
            let r = &self.repair;
            if r.rumble.on {
                push("highpass", r.enabled, &[("cutoff", f(rumble_cutoff(r.rumble.amount)))]);
            }
            if r.noise.on {
                push("denoise", r.enabled, &[("amount", f(r.noise.amount * 10.0))]);
            }
            if r.dehum.on {
                push("dehummer", r.enabled, &[("freq", ParamValue::Choice(u32::from(r.hum_hz == 60))), ("gain", f(-r.dehum.amount * 8.0))]);
            }
            if r.deess.on {
                let a = r.deess.amount;
                push("deesser", r.enabled, &[("threshold", f(-6.0 - a * 1.2)), ("reduction", f(a * 2.4))]);
            }
            if r.reverb.on {
                push("dereverb", r.enabled, &[("amount", f(r.reverb.amount * 10.0))]);
            }
        }
        if k.has(Section::Clarity) {
            let c = &self.clarity;
            if c.dynamics.on {
                let a = c.dynamics.amount;
                push("dynamics", c.enabled, &[("threshold", f(-12.0 - a * 2.4)), ("ratio", f(1.5 + a * 0.45)), ("attack", f(5.0)), ("release", f(120.0))]);
            }
            if c.eq.on {
                let p = eq_preset(&c.eq_preset).unwrap_or(&EQ_PRESETS[0]);
                let s = c.eq.amount / 10.0;
                push(
                    "parametric_eq",
                    c.enabled,
                    &[
                        ("low_freq", f(p.low.0)),
                        ("low_gain", f(p.low.1 * s)),
                        ("mid_freq", f(p.mid.0)),
                        ("mid_gain", f(p.mid.1 * s)),
                        ("mid_q", f(p.mid_q)),
                        ("high_freq", f(p.high.0)),
                        ("high_gain", f(p.high.1 * s)),
                    ],
                );
            }
            if c.enhance.on {
                push("speech_enhance", c.enabled, &[("mix", f(c.enhance.amount * 10.0)), ("tone", ParamValue::Choice(c.enhance_tone))]);
            }
        }
        if k.has(Section::Creative) {
            let c = &self.creative;
            if c.width.on && k == AudioType::Ambience {
                push("stereo_width", c.enabled, &[("width", f(100.0 + c.width.amount * 10.0))]);
            }
            if c.reverb.on {
                let p = reverb_preset(&c.reverb_preset).unwrap_or(&REVERB_PRESETS[0]);
                push(
                    "studio_reverb",
                    c.enabled,
                    &[("room", f(p.room)), ("decay", f(p.decay)), ("damping", f(p.damping)), ("dry", f(100.0)), ("wet", f(c.reverb.amount * 8.0))],
                );
            }
        }
        out
    }
}

fn snake(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            o.push('_');
            o.push(c.to_ascii_lowercase());
        } else {
            o.push(c);
        }
    }
    o
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "a boolean",
        Value::Number(_) | Value::Null => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a setting inside this group",
    }
}

/// Reduce Rumble amount (0…10) → highpass cutoff (30…150 Hz).
pub fn rumble_cutoff(amount: f64) -> f64 {
    30.0 + amount * 12.0
}

/// Canonical processing order of the effects Essential Sound manages.
pub const EFFECT_ORDER: &[&str] =
    &["highpass", "denoise", "dehummer", "deesser", "dereverb", "dynamics", "parametric_eq", "speech_enhance", "stereo_width", "studio_reverb"];

fn rank(id: &str) -> usize {
    EFFECT_ORDER.iter().position(|e| *e == id).unwrap_or(EFFECT_ORDER.len())
}

/// Reconcile `item`'s essential effects, clip gain, Volume and Panner with `item.essential`,
/// given the settings before the change (`old`; `None` = nothing was applied yet). Parameters
/// whose derived value did not change are left alone; changed ones are written at media time `mt`
/// (as a keyframe when the parameter is animated).
pub fn apply(item: &mut TrackItem, old: Option<&EssentialSound>, mt: Tick) {
    let new = item.essential.clone();
    let want = new.as_ref().map(EssentialSound::effects).unwrap_or_default();
    let had = old.map(EssentialSound::effects).unwrap_or_default();
    // remove essential effects no longer wanted
    item.effects.retain(|e| !e.essential || want.iter().any(|w| w.effect == e.effect));
    for w in &want {
        match item.effects.iter().position(|e| e.essential && e.effect == w.effect) {
            Some(i) => {
                let prev = had.iter().find(|h| h.effect == w.effect);
                let e = &mut item.effects[i];
                e.enabled = w.enabled;
                for (pid, p) in &w.params {
                    let changed = prev.and_then(|h| h.params.get(pid)).is_none_or(|hp| hp.value != p.value);
                    if changed && let Some(dst) = e.params.get_mut(pid) {
                        dst.set_at(mt, p.value.clone());
                    }
                }
            }
            None => {
                // after the last essential effect of a lower rank, else before the first other
                // standard effect
                let r = rank(&w.effect);
                let first_std = item.effects.iter().position(|e| !e.def().is_some_and(|d| d.intrinsic)).unwrap_or(item.effects.len());
                let pos = item.effects.iter().rposition(|e| e.essential && rank(&e.effect) < r).map(|i| i + 1).unwrap_or(first_std);
                item.effects.insert(pos, w.clone());
            }
        }
    }
    // clip gain (Auto-Match), Volume offset, Pan
    let g_old = old.map(EssentialSound::loudness_gain_db).unwrap_or(0.0);
    let g_new = new.as_ref().map(EssentialSound::loudness_gain_db).unwrap_or(0.0);
    if (g_new - g_old).abs() > 1e-12 {
        item.gain_db = (item.gain_db + g_new - g_old).clamp(-96.0, 96.0);
    }
    let v_old = old.map(EssentialSound::volume_offset_db).unwrap_or(0.0);
    let v_new = new.as_ref().map(EssentialSound::volume_offset_db).unwrap_or(0.0);
    if (v_new - v_old).abs() > 1e-12
        && let Some(p) = item.effect_mut("volume").and_then(|e| e.param_mut("level"))
    {
        offset_param(p, v_new - v_old);
    }
    let p_old = old.and_then(EssentialSound::pan_value);
    let p_new = new.as_ref().and_then(EssentialSound::pan_value);
    if p_new != p_old
        && let Some(p) = item.effect_mut("panner").and_then(|e| e.param_mut("balance"))
    {
        p.set_at(mt, ParamValue::Float(p_new.unwrap_or(0.0)));
    }
}

/// Add `db` to a level parameter: the static value, or every keyframe (keeps ducking shapes).
pub fn offset_param(p: &mut Param, db: f64) {
    let add = |v: &mut ParamValue| {
        if let ParamValue::Float(x) = v {
            *x = (*x + db).clamp(-287.5, 15.0);
        }
    };
    add(&mut p.value);
    for k in &mut p.keyframes {
        add(&mut k.value);
    }
}

// ------------------------------------------------------------------------------------- presets

/// An EQ preset for Clarity ▸ EQ: (frequency Hz, gain dB at full amount) per band. Values are our
/// own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqPreset {
    pub name: &'static str,
    pub low: (f64, f64),
    pub mid: (f64, f64),
    pub mid_q: f64,
    pub high: (f64, f64),
}

pub const EQ_PRESETS: &[EqPreset] = &[
    EqPreset { name: "Balanced Voice", low: (120.0, -3.0), mid: (3000.0, 3.0), mid_q: 1.0, high: (10000.0, 2.0) },
    EqPreset { name: "Presence Lift", low: (100.0, 0.0), mid: (4000.0, 5.0), mid_q: 0.8, high: (12000.0, 1.0) },
    EqPreset { name: "Warm Voice", low: (200.0, 3.0), mid: (3500.0, 1.5), mid_q: 1.0, high: (9000.0, -2.0) },
    EqPreset { name: "Clear Muddiness", low: (100.0, -2.0), mid: (300.0, -6.0), mid_q: 1.4, high: (10000.0, 1.0) },
    EqPreset { name: "Soften Harshness", low: (100.0, 0.0), mid: (3200.0, -5.0), mid_q: 1.2, high: (9000.0, -2.0) },
    EqPreset { name: "Bright Air", low: (100.0, 0.0), mid: (3000.0, 1.0), mid_q: 1.0, high: (8000.0, 6.0) },
    EqPreset { name: "Radio Voice", low: (150.0, -10.0), mid: (2500.0, 6.0), mid_q: 1.0, high: (6000.0, -8.0) },
    EqPreset { name: "Telephone", low: (400.0, -18.0), mid: (1500.0, 6.0), mid_q: 0.7, high: (3500.0, -18.0) },
];

pub fn eq_preset(name: &str) -> Option<&'static EqPreset> {
    EQ_PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// A reverb preset for Creative ▸ Reverb (Studio Reverb room / decay / damping, %). Our own values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbPreset {
    pub name: &'static str,
    pub room: f64,
    pub decay: f64,
    pub damping: f64,
}

pub const REVERB_PRESETS: &[ReverbPreset] = &[
    ReverbPreset { name: "Small Room", room: 25.0, decay: 25.0, damping: 60.0 },
    ReverbPreset { name: "Warm Room", room: 40.0, decay: 35.0, damping: 80.0 },
    ReverbPreset { name: "Medium Room", room: 50.0, decay: 45.0, damping: 50.0 },
    ReverbPreset { name: "Bright Studio", room: 45.0, decay: 40.0, damping: 15.0 },
    ReverbPreset { name: "Large Hall", room: 85.0, decay: 70.0, damping: 40.0 },
    ReverbPreset { name: "Stone Church", room: 100.0, decay: 92.0, damping: 30.0 },
    ReverbPreset { name: "Open Air", room: 90.0, decay: 15.0, damping: 70.0 },
];

pub fn reverb_preset(name: &str) -> Option<&'static ReverbPreset> {
    REVERB_PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// A named Essential Sound preset for one audio type: settings applied on top of the type's
/// defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub kind: AudioType,
    pub settings: EssentialSound,
    /// Built in (cannot be deleted).
    pub builtin: bool,
}

/// The built-in presets (our own names and values), "(Default)" first for each type.
pub fn builtin_presets() -> Vec<Preset> {
    use AudioType::*;
    let mk = |kind: AudioType, name: &str, f: &dyn Fn(&mut EssentialSound)| {
        let mut s = EssentialSound::new(kind);
        f(&mut s);
        s.preset = name.to_string();
        Preset { name: name.to_string(), kind, settings: s, builtin: true }
    };
    vec![
        mk(Dialogue, DEFAULT_PRESET, &|_| {}),
        mk(Dialogue, "Balanced Voice", &|s| {
            s.repair.rumble = Slot::at(4.0);
            s.clarity.dynamics = Slot::at(4.0);
            s.clarity.eq = Slot::at(5.0);
            s.clarity.eq_preset = "Balanced Voice".into();
        }),
        mk(Dialogue, "Clean Up Noisy Dialogue", &|s| {
            s.repair.noise = Slot::at(6.0);
            s.repair.rumble = Slot::at(5.0);
            s.repair.dehum = Slot::at(5.0);
            s.clarity.dynamics = Slot::at(3.0);
        }),
        mk(Dialogue, "Podcast Voice", &|s| {
            s.repair.rumble = Slot::at(5.0);
            s.repair.deess = Slot::at(4.0);
            s.clarity.dynamics = Slot::at(6.0);
            s.clarity.eq = Slot::at(6.0);
            s.clarity.eq_preset = "Presence Lift".into();
            s.clarity.enhance = Slot::at(7.0);
        }),
        mk(Dialogue, "Dry Up a Roomy Voice", &|s| {
            s.repair.reverb = Slot::at(6.0);
            s.repair.rumble = Slot::at(4.0);
            s.clarity.eq = Slot::at(4.0);
            s.clarity.eq_preset = "Clear Muddiness".into();
        }),
        mk(Dialogue, "Phone Call", &|s| {
            s.clarity.eq = Slot::at(10.0);
            s.clarity.eq_preset = "Telephone".into();
            s.clarity.dynamics = Slot::at(5.0);
        }),
        mk(Dialogue, "Radio Announcer", &|s| {
            s.clarity.eq = Slot::at(8.0);
            s.clarity.eq_preset = "Radio Voice".into();
            s.clarity.dynamics = Slot::at(8.0);
            s.repair.deess = Slot::at(5.0);
        }),
        mk(Dialogue, "Voice in a Hall", &|s| {
            s.creative.reverb = Slot::at(5.0);
            s.creative.reverb_preset = "Large Hall".into();
        }),
        mk(Music, DEFAULT_PRESET, &|_| {}),
        mk(Music, "Duck Under Dialogue", &|s| {
            s.ducking.enabled = true;
        }),
        mk(Music, "Duck Deep Under Dialogue", &|s| {
            s.ducking.enabled = true;
            s.ducking.reduce_db = -24.0;
            s.ducking.fade_s = 1.2;
        }),
        mk(Music, "Duck Under Voice and Effects", &|s| {
            s.ducking.enabled = true;
            s.ducking.against = vec![Dialogue, Sfx];
            s.ducking.reduce_db = -12.0;
        }),
        mk(Sfx, DEFAULT_PRESET, &|_| {}),
        mk(Sfx, "From the Left", &|s| {
            s.pan.enabled = true;
            s.pan.value = -60.0;
        }),
        mk(Sfx, "From the Right", &|s| {
            s.pan.enabled = true;
            s.pan.value = 60.0;
        }),
        mk(Sfx, "In a Room", &|s| {
            s.creative.reverb = Slot::at(4.0);
            s.creative.reverb_preset = "Small Room".into();
        }),
        mk(Sfx, "Far Away", &|s| {
            s.creative.reverb = Slot::at(8.0);
            s.creative.reverb_preset = "Open Air".into();
        }),
        mk(Ambience, DEFAULT_PRESET, &|_| {}),
        mk(Ambience, "Wide Outdoor", &|s| {
            s.creative.width = Slot::at(7.0);
        }),
        mk(Ambience, "Interior Room Tone", &|s| {
            s.creative.reverb = Slot::at(3.0);
            s.creative.reverb_preset = "Warm Room".into();
        }),
        mk(Ambience, "Duck Under Dialogue", &|s| {
            s.ducking.enabled = true;
        }),
    ]
}

/// Apply a preset's settings to a clip's state, keeping what a preset does not own (the
/// measured loudness and its match gain, Clip Volume, Mute).
pub fn with_preset(current: Option<&EssentialSound>, preset: &Preset) -> EssentialSound {
    let mut s = preset.settings.clone();
    s.preset = preset.name.clone();
    if let Some(c) = current.filter(|c| c.kind == preset.kind) {
        s.loudness.measured_lufs = c.loudness.measured_lufs;
        s.loudness.target_lufs = c.loudness.target_lufs;
        s.loudness.gain_db = c.loudness.gain_db;
        s.volume = c.volume.clone();
        s.mute = c.mute;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::intrinsic_audio;
    use serde_json::json;

    fn clip() -> TrackItem {
        TrackItem {
            id: crate::ClipId(1),
            item: crate::ItemId(2),
            name: "d".into(),
            label: crate::Label::Iris,
            start: Tick::ZERO,
            duration: Tick(1000),
            source_in: Tick::ZERO,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: intrinsic_audio(),
            markers: vec![],
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        }
    }

    fn ids(it: &TrackItem) -> Vec<String> {
        it.effects.iter().map(|e| e.effect.clone()).collect()
    }

    #[test]
    fn set_get_and_validation() {
        let mut s = EssentialSound::new(AudioType::Dialogue);
        s.set("repair.noise.on", &json!(true)).unwrap();
        s.set("repair.noise.amount", &json!(42.0)).unwrap();
        assert_eq!(s.repair.noise, Slot { on: true, amount: 10.0 });
        s.set("clarity.eqPreset", &json!("Telephone")).unwrap();
        assert_eq!(s.get("clarity.eqPreset"), Some(json!("Telephone")));
        assert!(s.set("clarity.eqPreset", &json!("Nope")).is_err());
        assert!(s.set("repair.noise.on", &json!(3)).is_err());
        assert!(s.set("nope", &json!(3)).is_err());
        assert!(s.set("kind", &json!("Music")).is_err());
        assert!(s.preset.is_empty(), "a manual change leaves the preset");
        s.set("ducking.against", &json!(["Dialogue", "SFX"])).unwrap();
        assert_eq!(s.ducking.against, vec![AudioType::Dialogue, AudioType::Sfx]);
    }

    #[test]
    fn apply_inserts_updates_and_removes_effects_in_order() {
        let mut it = clip();
        it.effects.push(find_effect("delay").unwrap().instance());
        let mut s = EssentialSound::new(AudioType::Dialogue);
        s.repair.deess = Slot::at(5.0);
        s.creative.reverb = Slot::at(5.0);
        s.repair.rumble = Slot::at(5.0);
        it.essential = Some(s.clone());
        apply(&mut it, None, Tick::ZERO);
        assert_eq!(ids(&it), ["volume", "channel_volume", "panner", "highpass", "deesser", "studio_reverb", "delay"]);
        // keyframe a parameter by hand, then change an unrelated slider: keyframes survive
        let k = it.effects.iter_mut().find(|e| e.effect == "deesser").unwrap();
        k.params.get_mut("reduction").unwrap().put_keyframe(Tick(10), ParamValue::Float(3.0));
        let old = s.clone();
        s.repair.rumble.amount = 10.0;
        s.repair.noise = Slot::at(2.0);
        it.essential = Some(s.clone());
        apply(&mut it, Some(&old), Tick::ZERO);
        assert_eq!(ids(&it), ["volume", "channel_volume", "panner", "highpass", "denoise", "deesser", "studio_reverb", "delay"]);
        assert_eq!(it.effect("highpass").unwrap().f64_at("cutoff", Tick::ZERO), 150.0);
        assert!(it.effect("deesser").unwrap().param("reduction").unwrap().is_animated());
        // section off bypasses, slot off removes
        let old = s.clone();
        s.repair.enabled = false;
        s.creative.reverb.on = false;
        it.essential = Some(s.clone());
        apply(&mut it, Some(&old), Tick::ZERO);
        assert!(!it.effect("highpass").unwrap().enabled);
        assert!(it.effect("studio_reverb").is_none());
        // clearing removes every essential effect but keeps the user's
        let old = s.clone();
        it.essential = None;
        apply(&mut it, Some(&old), Tick::ZERO);
        assert_eq!(ids(&it), ["volume", "channel_volume", "panner", "delay"]);
    }

    #[test]
    fn gain_volume_and_pan_are_deltas() {
        let mut it = clip();
        it.gain_db = 2.0;
        let mut s = EssentialSound::new(AudioType::Sfx);
        s.loudness.gain_db = -5.0;
        s.volume = ClipVolume { on: true, level_db: -3.0 };
        s.pan = Pan { enabled: true, value: 40.0 };
        it.essential = Some(s.clone());
        apply(&mut it, None, Tick::ZERO);
        assert_eq!(it.gain_db, -3.0);
        assert_eq!(it.effect("volume").unwrap().f64_at("level", Tick::ZERO), -3.0);
        assert_eq!(it.effect("panner").unwrap().f64_at("balance", Tick::ZERO), 40.0);
        let old = s.clone();
        it.essential = None;
        apply(&mut it, Some(&old), Tick::ZERO);
        assert_eq!(it.gain_db, 2.0);
        assert_eq!(it.effect("volume").unwrap().f64_at("level", Tick::ZERO), 0.0);
        assert_eq!(it.effect("panner").unwrap().f64_at("balance", Tick::ZERO), 0.0);
    }

    #[test]
    fn presets_are_valid_and_unique() {
        let ps = builtin_presets();
        for t in AudioType::ALL {
            let mine: Vec<_> = ps.iter().filter(|p| p.kind == t).collect();
            assert_eq!(mine[0].name, DEFAULT_PRESET);
            let mut names: Vec<_> = mine.iter().map(|p| p.name.as_str()).collect();
            names.dedup();
            assert_eq!(names.len(), mine.len());
        }
        for p in &ps {
            assert!(eq_preset(&p.settings.clarity.eq_preset).is_some());
            assert!(reverb_preset(&p.settings.creative.reverb_preset).is_some());
            for e in p.settings.effects() {
                assert!(e.def().is_some(), "{}: unknown effect {}", p.name, e.effect);
            }
        }
        // serde round trip
        let s = &ps[3].settings;
        let back: EssentialSound = serde_json::from_value(s.to_value()).unwrap();
        assert_eq!(&back, s);
    }
}
