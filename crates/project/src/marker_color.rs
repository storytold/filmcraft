//! Exact custom marker colors, serialized as canonical sRGB hex strings.
use crate::Marker;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HexColor(pub [u8; 3]);

impl HexColor {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('#').unwrap_or(text);
        if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok();
        Some(Self([byte(0)?, byte(2)?, byte(4)?]))
    }

    pub fn hex(self) -> String {
        let [r, g, b] = self.0;
        format!("#{r:02X}{g:02X}{b:02X}")
    }
}

impl Serialize for HexColor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| serde::de::Error::custom("expected a six-digit sRGB hex color (#RRGGBB)"))
    }
}

impl Marker {
    pub fn rgb(&self) -> [u8; 3] {
        self.custom_color.map_or_else(|| self.color.marker_rgb(), |c| c.0)
    }

    /// Preset name or exact custom hex, for commands and reports.
    pub fn color_value(&self) -> String {
        self.custom_color.map_or_else(|| self.color.name().to_string(), HexColor::hex)
    }
}

impl crate::ProjectItem {
    pub fn label_value(&self) -> String {
        self.custom_label_color.map_or_else(|| self.label.name().to_string(), HexColor::hex)
    }
}

impl crate::TrackItem {
    pub fn label_value(&self) -> String {
        self.custom_label_color.map_or_else(|| self.label.name().to_string(), HexColor::hex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip_and_hostile_input() {
        let color: HexColor = serde_json::from_str("\"#aBcD09\"").unwrap();
        assert_eq!(color.0, [171, 205, 9]);
        assert_eq!(serde_json::to_string(&color).unwrap(), "\"#ABCD09\"");
        for s in ["", "#FFF", "#12345678", "#GG0000", "€abc", "000000 "] {
            assert!(HexColor::parse(s).is_none(), "{s}");
        }
    }
    #[test]
    fn legacy_marker_keeps_preset_and_has_no_invented_author() {
        let value = serde_json::json!({"id":1,"start":0,"duration":0,"name":"Old", "comment":"", "kind":"Comment", "color":"Magenta"});
        let marker: Marker = serde_json::from_value(value).unwrap();
        assert_eq!(marker.custom_color, None);
        assert_eq!(marker.author, "");
        assert_eq!(marker.rgb(), crate::Label::Magenta.marker_rgb());
    }
}
