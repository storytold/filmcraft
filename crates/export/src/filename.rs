//! Output filename validation shared by export preflight and the UI (#1158).
//! Native directory separators and drive prefixes belong to the path, not its filename.

use std::path::Path;

const INVALID_CHARACTERS: &str = "File name cannot contain < > : \" / \\ | ? * or control characters.";
const MISSING_FILENAME: &str = "Choose an output file name.";

/// Reject the unsupported filename characters reported for movie exports.
/// Empty editor text is allowed: Export mode supplies its existing default name.
pub fn validate_output_filename(name: &str) -> std::result::Result<(), &'static str> {
    if name.chars().any(|c| c < '\u{20}' || r#"<>:"/\|?*"#.contains(c)) { Err(INVALID_CHARACTERS) } else { Ok(()) }
}

/// Validate only the filename component of a native output path.
pub fn validate_output_path(path: &str) -> std::result::Result<(), &'static str> {
    let name = Path::new(path).file_name().and_then(|n| n.to_str()).ok_or(MISSING_FILENAME)?;
    validate_output_filename(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_filename_characters_are_rejected_without_renaming() {
        for c in ['<', '>', ':', '"', '/', '\\', '|', '?', '*', '\0', '\u{1f}'] {
            assert!(validate_output_filename(&format!("movie{c}draft.mp4")).is_err(), "{c:?}");
        }
        for name in ["", "movie draft.mp4", "映像 café 🎬.mp4", "frame_%04d.png", ".hidden.mp4"] {
            assert!(validate_output_filename(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn native_paths_keep_directory_separators_and_image_sequence_placeholders() {
        let relative = Path::new("exports").join("映像 final 100%.mp4");
        assert!(validate_output_path(relative.to_str().unwrap()).is_ok());
        assert!(validate_output_path("exports/frame_%04d.png").is_ok());
        for path in ["", ".", ".."] {
            assert!(validate_output_path(path).is_err());
        }
        assert!(validate_output_path("exports/movie?draft.mp4").is_err());
        #[cfg(windows)]
        for path in [r"C:\Movies\映像 final.mp4", r"\\server\share\frame_%04d.png", r"\\?\C:\Movies\movie.mp4"] {
            assert!(validate_output_path(path).is_ok(), "{path}");
        }
        #[cfg(not(windows))]
        {
            assert!(validate_output_path("/exports/folder:allowed/映像 final.mp4").is_ok());
            assert!(validate_output_path(r"exports/movie\draft.mp4").is_err());
        }
    }
}
