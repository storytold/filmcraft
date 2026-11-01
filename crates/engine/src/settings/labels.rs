use super::{BTreeMap, Deserialize, Label, Path, PathBuf, Serialize, Session};

/// One editable label colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LabelColor {
    pub name: String,
    /// `#rrggbb`.
    pub color: String,
}

impl Default for LabelColor {
    fn default() -> Self {
        Self { name: String::new(), color: "#808080".into() }
    }
}

/// Settings ▸ Labels ▸ Label Defaults (which label new items get).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LabelDefaults {
    pub movie: Label,
    pub video: Label,
    pub audio: Label,
    pub still: Label,
    pub sequence: Label,
    pub dynamic_link: Label,
    pub bin: Label,
    pub captions: Label,
}

impl Default for LabelDefaults {
    fn default() -> Self {
        Self {
            movie: Label::Iris,
            video: Label::Violet,
            audio: Label::Caribbean,
            still: Label::Lavender,
            sequence: Label::Forest,
            dynamic_link: Label::Rose,
            bin: Label::Mango,
            captions: Label::Mango,
        }
    }
}

/// Settings ▸ Labels: names and colours of the 16 labels (keyed by the label's id, e.g.
/// `labels.colors.violet.name`) and the label defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LabelPrefs {
    pub colors: BTreeMap<String, LabelColor>,
    pub defaults: LabelDefaults,
    /// Label Color Presets: the preset the names and colours were last taken from.
    pub preset: String,
}

impl Default for LabelPrefs {
    fn default() -> Self {
        Self {
            colors: Label::ALL.iter().map(|l| (label_id(*l), default_label_color(*l))).collect(),
            defaults: LabelDefaults::default(),
            preset: DEFAULT_LABEL_PRESET.into(),
        }
    }
}

/// The key of a label in [`LabelPrefs::colors`] (`violet`, `iris`, …).
pub fn label_id(l: Label) -> String {
    l.name().to_ascii_lowercase()
}

fn default_label_color(l: Label) -> LabelColor {
    LabelColor { name: l.name().into(), color: hex(l.rgb()) }
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `#rrggbb` (or `rrggbb`) → bytes.
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([p(0)?, p(2)?, p(4)?])
}

impl LabelPrefs {
    /// The colour of label `l` (the user's, else the built-in one).
    pub fn rgb(&self, l: Label) -> [u8; 3] {
        self.colors.get(&label_id(l)).and_then(|c| parse_hex(&c.color)).unwrap_or_else(|| l.rgb())
    }
    /// The display name of label `l`.
    pub fn name(&self, l: Label) -> String {
        self.colors.get(&label_id(l)).map(|c| c.name.trim()).filter(|n| !n.is_empty()).unwrap_or(l.name()).to_string()
    }
    /// The label a new media item of `kind` gets.
    pub fn for_media(&self, kind: filmcraft_media::MediaKind, has_video: bool, has_audio: bool) -> Label {
        use filmcraft_media::MediaKind as K;
        match kind {
            K::AudioOnly => self.defaults.audio,
            K::Still | K::ImageSequence => self.defaults.still,
            _ if has_video && !has_audio => self.defaults.video,
            _ if !has_video && has_audio => self.defaults.audio,
            _ => self.defaults.movie,
        }
    }
    pub(crate) fn sanitize_labels(&mut self) {
        for l in Label::ALL {
            let c = self.colors.entry(label_id(l)).or_insert_with(|| default_label_color(l));
            if parse_hex(&c.color).is_none() {
                c.color = hex(l.rgb());
            }
            c.name = c.name.chars().take(64).collect();
        }
        self.colors.retain(|k, _| Label::ALL.iter().any(|l| label_id(*l) == *k));
        self.preset = self.preset.chars().take(LABEL_NAME_MAX).collect();
    }
}

/// The label colour preset FilmCraft ships with: the labels' own names and colours.
pub const DEFAULT_LABEL_PRESET: &str = "Default";

/// The folder of label colour presets, beside `preferences.json`.
pub const LABEL_PRESETS_DIR: &str = "Label Presets";

/// Largest label preset file read; a real one is under 2 KiB.
const LABEL_PRESET_MAX_BYTES: u64 = 64 * 1024;

/// Longest preset or label name, in characters.
const LABEL_NAME_MAX: usize = 64;

/// Most files looked at in the presets folder.
const LABEL_PRESETS_MAX: usize = 500;

/// A label colour preset (Settings ▸ Labels ▸ Label Color Presets), shared between editors as a
/// small JSON file: its name and the 16 labels' names and colours, in [`Label::ALL`] order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LabelPreset {
    pub name: String,
    pub labels: Vec<PresetLabel>,
}

/// One label of a [`LabelPreset`]. A file must give both fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetLabel {
    pub name: String,
    /// `#rrggbb`.
    pub color: String,
}

/// The built-in presets after [`DEFAULT_LABEL_PRESET`]: (name, 16 × (label name, colour)) in
/// [`Label::ALL`] order. "Roles" puts the label defaults (Video, Movie, Audio, Still, Sequence,
/// Dynamic Link, Bin / Captions on labels 0, 1, 2, 3, 5, 6, 7) on matching roles.
const BUILTIN_LABEL_PRESETS: [(&str, [(&str, &str); 16]); 2] = [
    (
        "Bright",
        [
            ("Red", "#e3342f"),
            ("Orange", "#ec7a24"),
            ("Amber", "#f0a82a"),
            ("Yellow", "#e8d422"),
            ("Lime", "#9ed62a"),
            ("Green", "#36c24a"),
            ("Mint", "#2fd69a"),
            ("Cyan", "#24c9dc"),
            ("Sky", "#2fa0ec"),
            ("Blue", "#3d6ff0"),
            ("Indigo", "#5b5cf0"),
            ("Violet", "#8452ec"),
            ("Purple", "#ac46e0"),
            ("Magenta", "#dc3ccc"),
            ("Pink", "#ec4f8e"),
            ("Gray", "#8c8c8c"),
        ],
    ),
    (
        "Roles",
        [
            ("B-Roll", "#5b8fd4"),
            ("Footage", "#36a9c9"),
            ("Dialogue", "#4cb87a"),
            ("Graphics", "#e8913a"),
            ("Interview", "#d9c040"),
            ("Sequence", "#7f9e3c"),
            ("Visual Effects", "#c95bc4"),
            ("Bin", "#8a94a6"),
            ("Titles", "#e86f5c"),
            ("Music", "#8c6fe0"),
            ("Sound Effects", "#e0609a"),
            ("Voice-Over", "#5fd1b5"),
            ("Captions", "#c8b48a"),
            ("Ambience", "#a8754f"),
            ("Color Grade", "#d64545"),
            ("Stock", "#b5d454"),
        ],
    ),
];

/// Whether `name` is a built-in preset's (any case): built-ins can't be deleted or replaced.
pub fn is_builtin_label_preset(name: &str) -> bool {
    name.eq_ignore_ascii_case(DEFAULT_LABEL_PRESET) || BUILTIN_LABEL_PRESETS.iter().any(|(n, _)| name.eq_ignore_ascii_case(n))
}

impl LabelPreset {
    /// The built-in preset ([`DEFAULT_LABEL_PRESET`]).
    pub fn builtin() -> Self {
        Self::from_prefs(DEFAULT_LABEL_PRESET, &LabelPrefs::default())
    }

    /// Every built-in preset, [`DEFAULT_LABEL_PRESET`] first.
    pub fn builtins() -> Vec<Self> {
        let more = BUILTIN_LABEL_PRESETS.iter().map(|(name, labels)| Self {
            name: (*name).into(),
            labels: labels.iter().map(|(n, c)| PresetLabel { name: (*n).into(), color: (*c).into() }).collect(),
        });
        std::iter::once(Self::builtin()).chain(more).collect()
    }

    /// `labels`' names and colours as a preset called `name`.
    pub fn from_prefs(name: &str, labels: &LabelPrefs) -> Self {
        let entry = |l: Label| labels.colors.get(&label_id(l)).cloned().unwrap_or_else(|| default_label_color(l));
        Self { name: name.into(), labels: Label::ALL.iter().map(|l| entry(*l)).map(|c| PresetLabel { name: c.name, color: c.color }).collect() }
    }

    /// Whether `labels` has exactly this preset's names and colours.
    pub fn matches(&self, labels: &LabelPrefs) -> bool {
        Self::from_prefs(&self.name, labels) == *self
    }

    /// Give `labels` this preset's names and colours (the label defaults stay) and select it.
    pub fn apply(&self, labels: &mut LabelPrefs) {
        for (l, p) in Label::ALL.iter().zip(&self.labels) {
            labels.colors.insert(label_id(*l), LabelColor { name: p.name.clone(), color: p.color.clone() });
        }
        labels.preset = self.name.clone();
    }

    /// Check a preset file: at most 64 KiB of JSON with a name and exactly 16 labels, each with a
    /// name (trimmed, at most 64 characters) and a `#rrggbb` colour.
    pub fn parse(bytes: &[u8]) -> std::result::Result<Self, String> {
        if bytes.len() as u64 > LABEL_PRESET_MAX_BYTES {
            return Err("the file is too large to be a label color preset".into());
        }
        let mut p: Self = serde_json::from_slice(bytes).map_err(|e| format!("not a label color preset: {e}"))?;
        p.name = p.name.trim().to_string();
        if p.name.is_empty() || p.name.chars().count() > LABEL_NAME_MAX {
            return Err(format!("the preset needs a name of 1 to {LABEL_NAME_MAX} characters"));
        }
        if p.labels.len() != Label::ALL.len() {
            return Err(format!("a label color preset has {} labels; this file has {}", Label::ALL.len(), p.labels.len()));
        }
        for (i, l) in p.labels.iter_mut().enumerate() {
            l.name = l.name.trim().to_string();
            if l.name.chars().count() > LABEL_NAME_MAX {
                return Err(format!("label {}: the name is longer than {LABEL_NAME_MAX} characters", i + 1));
            }
            let rgb = l.color.strip_prefix('#').and(parse_hex(&l.color)).ok_or_else(|| format!("label {}: the color is not like #2f6bdf", i + 1))?;
            l.color = hex(rgb);
        }
        Ok(p)
    }

    /// Read and check a preset file ([`LabelPreset::parse`]).
    pub fn read(path: &Path) -> std::result::Result<Self, String> {
        use std::io::Read as _;
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(LABEL_PRESET_MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&bytes)
    }

    pub fn write(&self, path: &Path) -> std::result::Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        filmcraft_format::atomic_write(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Save `labels`' names and colours to `path` as a preset named after the file.
    pub fn export(path: &Path, labels: &LabelPrefs) -> std::result::Result<Self, String> {
        let stem = path.file_stem().map(|s| s.to_string_lossy().trim().chars().take(LABEL_NAME_MAX).collect::<String>()).unwrap_or_default();
        let p = Self::from_prefs(if stem.is_empty() { "Labels" } else { &stem }, labels);
        p.write(path)?;
        Ok(p)
    }
}

impl Session {
    /// The label colour presets folder (None without a data directory, e.g. tests and the web).
    pub fn label_presets_dir(&self) -> Option<PathBuf> {
        self.prefs_path.as_deref().and_then(Path::parent).map(|d| d.join(LABEL_PRESETS_DIR))
    }

    /// The built-in presets, then the presets in the folder by name with the file each is in.
    /// Files that fail [`LabelPreset::parse`] and names already taken are left out.
    pub fn label_presets(&self) -> Vec<(LabelPreset, Option<PathBuf>)> {
        let mut found: Vec<(LabelPreset, Option<PathBuf>)> = self
            .label_presets_dir()
            .and_then(|d| std::fs::read_dir(d).ok())
            .into_iter()
            .flatten()
            .flatten()
            .take(LABEL_PRESETS_MAX)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
            .filter_map(|p| LabelPreset::read(&p).ok().map(|preset| (preset, Some(p))))
            .collect();
        found.sort_by_key(|(p, _)| p.name.to_lowercase());
        let mut out: Vec<(LabelPreset, Option<PathBuf>)> = LabelPreset::builtins().into_iter().map(|p| (p, None)).collect();
        for f in found {
            if !out.iter().any(|(p, _)| p.name.eq_ignore_ascii_case(&f.0.name)) {
                out.push(f);
            }
        }
        out
    }

    /// Copy the preset file at `path` into the presets folder, replacing a preset of the same
    /// name, and return it. One named like a built-in preset becomes "<name> (imported)".
    pub fn import_label_preset(&self, path: &Path) -> std::result::Result<LabelPreset, String> {
        let mut p = LabelPreset::read(path)?;
        if is_builtin_label_preset(&p.name) {
            p.name = format!("{} (imported)", p.name);
        }
        let dir = self.label_presets_dir().ok_or("there is no presets folder in this session")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (old, file) in self.label_presets() {
            if let Some(file) = file.filter(|_| old.name.eq_ignore_ascii_case(&p.name)) {
                std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            }
        }
        let safe: String = p.name.chars().map(|c| if c.is_alphanumeric() || " -_()".contains(c) { c } else { '_' }).collect();
        p.write(&dir.join(format!("{}.json", safe.trim())))?;
        Ok(p)
    }

    /// Delete a preset from the presets folder (built-in presets can't be deleted).
    pub fn delete_label_preset(&self, name: &str) -> std::result::Result<(), String> {
        let (_, file) = self.label_presets().into_iter().find(|(p, _)| p.name == name).ok_or_else(|| format!("there is no label color preset “{name}”"))?;
        let file = file.ok_or("built-in presets can't be deleted")?;
        std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))
    }
}
