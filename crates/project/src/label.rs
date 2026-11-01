use super::{Deserialize, Serialize};

/// Premiere-style label colours (names from the Label menu; values are our own).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Label {
    Violet,
    Iris,
    Caribbean,
    Lavender,
    Cerulean,
    Forest,
    Rose,
    Mango,
    Purple,
    Blue,
    Teal,
    Magenta,
    Tan,
    Green,
    Brown,
    Yellow,
}

impl Label {
    pub const ALL: [Label; 16] = [
        Label::Violet,
        Label::Iris,
        Label::Caribbean,
        Label::Lavender,
        Label::Cerulean,
        Label::Forest,
        Label::Rose,
        Label::Mango,
        Label::Purple,
        Label::Blue,
        Label::Teal,
        Label::Magenta,
        Label::Tan,
        Label::Green,
        Label::Brown,
        Label::Yellow,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Label::Violet => "Violet",
            Label::Iris => "Iris",
            Label::Caribbean => "Caribbean",
            Label::Lavender => "Lavender",
            Label::Cerulean => "Cerulean",
            Label::Forest => "Forest",
            Label::Rose => "Rose",
            Label::Mango => "Mango",
            Label::Purple => "Purple",
            Label::Blue => "Blue",
            Label::Teal => "Teal",
            Label::Magenta => "Magenta",
            Label::Tan => "Tan",
            Label::Green => "Green",
            Label::Brown => "Brown",
            Label::Yellow => "Yellow",
        }
    }
    /// Label colours (Premiere 26 Spectrum defaults, measured; see plan/premiere/02-ui-ux.md §1.5).
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Label::Violet => [0x38, 0x0e, 0xa7],
            Label::Iris => [0x1d, 0x4a, 0x64],
            Label::Caribbean => [0x35, 0x54, 0x18],
            Label::Lavender => [0x6b, 0x1c, 0x82],
            Label::Cerulean => [0x23, 0x53, 0x5a],
            Label::Forest => [0x40, 0x4a, 0x11],
            Label::Rose => [0x80, 0x18, 0x36],
            Label::Mango => [0x80, 0x3f, 0x17],
            Label::Purple => [0x59, 0x0d, 0xb0],
            Label::Blue => [0x19, 0x2d, 0x94],
            Label::Teal => [0x1f, 0x4d, 0x45],
            Label::Magenta => [0x79, 0x1c, 0x56],
            Label::Tan => [0x6c, 0x5b, 0x47],
            Label::Green => [0x29, 0x5c, 0x2d],
            Label::Brown => [0x58, 0x3d, 0x14],
            Label::Yellow => [0x6e, 0x66, 0x28],
        }
    }
    /// Marker and clip defaults share the same sixteen preset colors.
    pub fn marker_rgb(self) -> [u8; 3] {
        self.rgb()
    }
    /// Alternate user-facing color names resolve to presets, not additional swatches.
    pub const COLOR_ALIASES: [(&'static str, Label); 4] = [("Red", Label::Rose), ("Orange", Label::Mango), ("Cyan", Label::Teal), ("Pink", Label::Magenta)];
    pub fn from_color_name(name: &str) -> Option<Label> {
        Self::from_name(name).or_else(|| Self::COLOR_ALIASES.iter().find(|(alias, _)| alias.eq_ignore_ascii_case(name)).map(|(_, label)| *label))
    }
    pub fn from_name(s: &str) -> Option<Label> {
        Self::ALL.iter().copied().find(|l| l.name().eq_ignore_ascii_case(s))
    }
}
