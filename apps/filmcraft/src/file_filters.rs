//! Extension filters for native file dialogs.
//!
//! Every filtered file dialog (import, relink, open file/preset, open/save project) builds
//! its filter with [`extensions`]. On Linux and the BSDs, rfd's XDG portal, GTK and Zenity
//! backends turn each extension into a case-sensitive `*.{ext}` glob, so camera files such
//! as `shot.MP4` or `shot.Mp4` would be hidden. The helper appends one bracket pattern per
//! extension (`mp4` -> `[mM][pP]4`) after the literal extensions, which stay first so save
//! dialogs keep their normal default suffix. macOS and Windows dialogs already ignore case
//! and receive the plain list.

pub fn extensions(exts: &[&str]) -> Vec<String> {
    let extensions: Vec<_> = exts.iter().map(|ext| (*ext).to_owned()).collect();
    // XDG portal/Zenity globs are case-sensitive. Keep the literal extensions first so
    // Save dialogs can still infer a normal default suffix, then cover mixed case too.
    // https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let mut extensions = extensions;
        for ext in exts {
            if !ext.bytes().any(|b| b.is_ascii_alphabetic()) {
                continue;
            }
            let mut pattern = String::new();
            for c in ext.chars() {
                if c.is_ascii_alphabetic() {
                    pattern.extend(['[', c.to_ascii_lowercase(), c.to_ascii_uppercase(), ']']);
                } else {
                    pattern.push(c);
                }
            }
            extensions.push(pattern);
        }
        extensions
    }
    // Native macOS and Windows filters take ordinary extensions.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    extensions
}

#[cfg(test)]
mod tests {
    use super::extensions;

    // rfd's XDG portal and Zenity backends turn each extension into `*.{ext}`.
    // Exercise those case-sensitive patterns against filenames, as the picker does.
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn camera_files_are_visible_in_every_capitalization() {
        let filters: Vec<_> = extensions(&["mp4", "mov", "m2ts", "wav", "fcproj"]).iter().map(|ext| glob::Pattern::new(&format!("*.{ext}")).unwrap()).collect();
        for name in ["camera.mp4", "camera.MP4", "camera.Mp4", "clip.MOV", "clip.moV", "take.M2TS", "sound.WaV", "Edit.FCPROJ", "Edit.FcProj"] {
            assert!(filters.iter().any(|f| f.matches(name)), "the picker hides {name}");
        }
        for name in ["camera.mp4.bak", "camera.mp44", "camera.xmp4", "notes.txt", "mp4"] {
            assert!(!filters.iter().any(|f| f.matches(name)), "the picker admits {name}");
        }
    }

    #[test]
    fn save_filters_keep_the_literal_default_extension() {
        assert_eq!(extensions(&["mp4", "mov"]).first().map(String::as_str), Some("mp4"));
        assert!(extensions(&[]).is_empty());
        assert_eq!(extensions(&["*", "", "123"]), ["*", "", "123"]);
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn native_filters_receive_only_literal_extensions() {
        assert_eq!(extensions(&["mp4", "mov", "fcproj"]), ["mp4", "mov", "fcproj"]);
    }
}
