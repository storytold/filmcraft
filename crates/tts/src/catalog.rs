//! The downloadable neural voices: Kokoro-82M, its voice packs and the CMUdict pronunciation
//! dictionary. Every file is pinned to a revision and checked against its SHA-256 when downloaded
//! (the downloader is `filmcraft_speech::models::download_files`, run by the engine after the
//! user confirms). Nothing here is bundled with FilmCraft or committed to the repository.
//!
//! Licences: Kokoro-82M weights and voices by hexgrad, Apache-2.0 (declared on the model card; the
//! repository has no separate LICENSE file). CMUdict, Carnegie Mellon University, BSD-style.
//! Pins verified 2026-10-07 against the Hugging Face API and by `sha256sum`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{Gender, VoiceInfo};

/// One file of the neural voice package.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelFile {
    /// Local file name inside the package directory.
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// Directory name of the package under the models directory.
pub const PACKAGE_ID: &str = "kokoro-82m";
pub const PACKAGE_NAME: &str = "Natural voices (Kokoro-82M)";
pub const LICENSE: &str = "Apache-2.0 (Kokoro-82M by hexgrad); CMUdict: BSD-style (Carnegie Mellon University)";
pub const SOURCE: &str = "https://huggingface.co/hexgrad/Kokoro-82M";

macro_rules! hf {
    ($name:literal, $path:literal, $sha:literal, $size:expr) => {
        ModelFile {
            name: $name,
            url: concat!("https://huggingface.co/hexgrad/Kokoro-82M/resolve/f3ff3571791e39611d31c381e3a41a3af07b4987/", $path),
            sha256: $sha,
            size: $size,
        }
    };
}

pub static FILES: &[ModelFile] = &[
    hf!("config.json", "config.json", "5abb01e2403b072bf03d04fde160443e209d7a0dad49a423be15196b9b43c17f", 2351),
    hf!("kokoro-v1_0.pth", "kokoro-v1_0.pth", "496dba118d1a58f5f3db2efc88dbdc216e0483fc89fe6e47ee1f2c53f18ad1e4", 327_212_226),
    ModelFile {
        name: "cmudict.dict",
        url: "https://raw.githubusercontent.com/cmusphinx/cmudict/74790861f652b15e4ac49015a90074ad62a27690/cmudict.dict",
        sha256: "81917843c7f44ce2b094ac63873c2c7a4cf802040792c455ba3ca406891c3d22",
        size: 3_618_488,
    },
    hf!("af_heart.pt", "voices/af_heart.pt", "0ab5709b8ffab19bfd849cd11d98f75b60af7733253ad0d67b12382a102cb4ff", 523_425),
    hf!("af_bella.pt", "voices/af_bella.pt", "8cb64e02fcc8de0327a8e13817e49c76c945ecf0052ceac97d3081480e8e48d6", 523_425),
    hf!("af_nicole.pt", "voices/af_nicole.pt", "c5561808bcf5250fe8c5f5de32caf2d94f27e57e95befdb098c5c85991d4c5da", 523_430),
    hf!("af_aoede.pt", "voices/af_aoede.pt", "c03bd1a4c3716c2d8eaa3d50022f62d5c31cfbd6e15933a00b17fefe13841cc4", 523_425),
    hf!("af_kore.pt", "voices/af_kore.pt", "8bfbc512321c3db49dff984ac675fa5ac7eaed5a96cc31104d3a9080e179d69d", 523_420),
    hf!("af_sarah.pt", "voices/af_sarah.pt", "49bd364ea3be9eb3e9685e8f9a15448c4883112a7c0ff7ab139fa4088b08cef9", 523_425),
    hf!("am_michael.pt", "voices/am_michael.pt", "9a443b79a4b22489a5b0ab7c651a0bcd1a30bef675c28333f06971abbd47bd37", 523_435),
    hf!("am_fenrir.pt", "voices/am_fenrir.pt", "98e507eca1db08230ae3b6232d59c10aec9630022d19accac4f5d12fcec3c37a", 523_430),
    hf!("am_puck.pt", "voices/am_puck.pt", "dd1d8973f4ce4b7d8ae407c77a435f485dabc052081b80ea75c4f30b84f36223", 523_420),
];

/// A neural voice: catalogue entry and its voice-pack file.
#[derive(Clone, Copy, Debug)]
pub struct NeuralVoice {
    pub info: VoiceInfo,
    pub file: &'static str,
}

const NEURAL_LICENSE: &str = "Apache-2.0 (Kokoro-82M by hexgrad)";

macro_rules! voice {
    ($id:literal, $name:literal, $gender:ident, $file:literal, $desc:literal) => {
        NeuralVoice {
            info: VoiceInfo {
                id: $id,
                name: $name,
                language: "en-US",
                gender: Gender::$gender,
                engine: "neural",
                description: $desc,
                license: NEURAL_LICENSE,
                installed: false,
            },
            file: $file,
        }
    };
}

/// US English voices the model card grades C+ or better, best first.
pub static VOICES: &[NeuralVoice] = &[
    voice!("kokoro-heart", "Heart", Female, "af_heart.pt", "Natural voice, warm (model card grade A)."),
    voice!("kokoro-bella", "Bella", Female, "af_bella.pt", "Natural voice, bright (grade A−)."),
    voice!("kokoro-nicole", "Nicole", Female, "af_nicole.pt", "Natural voice, soft (grade B−)."),
    voice!("kokoro-michael", "Michael", Male, "am_michael.pt", "Natural voice (grade C+)."),
    voice!("kokoro-fenrir", "Fenrir", Male, "am_fenrir.pt", "Natural voice, deep (grade C+)."),
    voice!("kokoro-puck", "Puck", Male, "am_puck.pt", "Natural voice, lively (grade C+)."),
    voice!("kokoro-aoede", "Aoede", Female, "af_aoede.pt", "Natural voice (grade C+)."),
    voice!("kokoro-kore", "Kore", Female, "af_kore.pt", "Natural voice (grade C+)."),
    voice!("kokoro-sarah", "Sarah", Female, "af_sarah.pt", "Natural voice (grade C+)."),
];

pub const PIPER_RU_PACKAGE_ID: &str = "piper-ru";
pub const PIPER_RU_PACKAGE_NAME: &str = "Russian natural voices (Piper ru_RU)";
pub const PIPER_RU_LICENSE: &str = "MIT (rhasspy/piper-voices by Michael Hansen)";
pub const PIPER_RU_SOURCE: &str = "https://huggingface.co/rhasspy/piper-voices";

pub static PIPER_RU_FILES: &[ModelFile] = &[
    ModelFile {
        name: "ru_RU-dmitri-medium.onnx",
        url: "https://huggingface.co/rhasspy/piper-voices/resolve/e7229f7d33a10e145e109b886d661984201ac3c4/ru/ru_RU/dmitri/medium/ru_RU-dmitri-medium.onnx",
        sha256: "f073356ebc4bd0f80c5af58df2953a5988bd5bdab1eb38635ce960b071fbefcb",
        size: 63_201_294,
    },
    ModelFile {
        name: "ru_RU-dmitri-medium.onnx.json",
        url: "https://huggingface.co/rhasspy/piper-voices/raw/e7229f7d33a10e145e109b886d661984201ac3c4/ru/ru_RU/dmitri/medium/ru_RU-dmitri-medium.onnx.json",
        sha256: "667ef3117bc642c2892dff7690d8bdc8ca4228aeaa783b2dc1416df632855e0d",
        size: 4_824,
    },
    ModelFile {
        name: "ru_RU-irina-medium.onnx",
        url: "https://huggingface.co/rhasspy/piper-voices/resolve/e7229f7d33a10e145e109b886d661984201ac3c4/ru/ru_RU/irina/medium/ru_RU-irina-medium.onnx",
        sha256: "8ff38212d23da300bbe3705c645e6e5b9475f0bfde01558eb17813e22acaaaaa",
        size: 63_201_294,
    },
    ModelFile {
        name: "ru_RU-irina-medium.onnx.json",
        url: "https://huggingface.co/rhasspy/piper-voices/raw/e7229f7d33a10e145e109b886d661984201ac3c4/ru/ru_RU/irina/medium/ru_RU-irina-medium.onnx.json",
        sha256: "c2ec28bb38e2b59e93b959b3e40348c1afebbd272f30fed5d41205d08e98a9d7",
        size: 4_765,
    },
];

pub static PIPER_RU_VOICES: &[NeuralVoice] = &[
    NeuralVoice {
        info: VoiceInfo {
            id: "piper-dmitri",
            name: "Dmitri",
            language: "ru-RU",
            gender: Gender::Male,
            engine: "neural",
            description: "Russian natural voice, Dmitri (medium quality, VITS).",
            license: PIPER_RU_LICENSE,
            installed: false,
        },
        file: "ru_RU-dmitri-medium.onnx",
    },
    NeuralVoice {
        info: VoiceInfo {
            id: "piper-irina",
            name: "Irina",
            language: "ru-RU",
            gender: Gender::Female,
            engine: "neural",
            description: "Russian natural voice, Irina (medium quality, VITS).",
            license: PIPER_RU_LICENSE,
            installed: false,
        },
        file: "ru_RU-irina-medium.onnx",
    },
];

pub const KOKORO_RU_PACKAGE_ID: &str = "kokoro-ru";
pub const KOKORO_RU_PACKAGE_NAME: &str = "Russian studio voices (Kokoro-RU 24 kHz)";
pub const KOKORO_RU_LICENSE: &str = "Apache-2.0 (Kokoro-82M by hexgrad, fine-tuned by zaakirio)";
pub const KOKORO_RU_SOURCE: &str = "https://huggingface.co/zaakirio/kokoro-ru";

pub static KOKORO_RU_FILES: &[ModelFile] = &[
    ModelFile {
        name: "config.json",
        url: "https://huggingface.co/zaakirio/kokoro-ru/resolve/d649c57b239b18c4c384378127cbf01dba039bc1/config.json",
        sha256: "e4da95d48ae0f937adeda8dd99a804941913327e6d7607a7a15de11abb69a120",
        size: 2542,
    },
    ModelFile {
        name: "kokoro-ru-v2-base.pth",
        url: "https://huggingface.co/zaakirio/kokoro-ru/resolve/d649c57b239b18c4c384378127cbf01dba039bc1/kokoro-ru-v2-base.pth",
        sha256: "3bbee5bc05cfa182afc365b9116eaed8355f939c3c0af8aa0e43fdc45343ca15",
        size: 327_220_619,
    },
    ModelFile {
        name: "sveta.pt",
        url: "https://huggingface.co/zaakirio/kokoro-ru/resolve/d649c57b239b18c4c384378127cbf01dba039bc1/voices/sveta.pt",
        sha256: "248c00e98f7ce20c31ff5537b52ea3d3b204a58845d86da73960f30f112f60a7",
        size: 523_831,
    },
    ModelFile {
        name: "dima.pt",
        url: "https://huggingface.co/zaakirio/kokoro-ru/resolve/d649c57b239b18c4c384378127cbf01dba039bc1/voices/dima.pt",
        sha256: "80d4d216b5906c232ede48f2c07ab3aee0785e34d1df6ec6dc8cf44767d74c69",
        size: 523_831,
    },
    ModelFile {
        name: "masha.pt",
        url: "https://huggingface.co/zaakirio/kokoro-ru/resolve/d649c57b239b18c4c384378127cbf01dba039bc1/voices/masha.pt",
        sha256: "464d64ae793647c3b3f9d1f08ce5b11c641a61799f0bca65f111e6aad5d0a362",
        size: 523_831,
    },
];

pub static KOKORO_RU_VOICES: &[NeuralVoice] = &[
    NeuralVoice {
        info: VoiceInfo {
            id: "kokoro-ru-sveta",
            name: "Sveta",
            language: "ru-RU",
            gender: Gender::Female,
            engine: "neural",
            description: "Russian studio natural voice, Sveta (24 kHz, StyleTTS2).",
            license: KOKORO_RU_LICENSE,
            installed: false,
        },
        file: "sveta.pt",
    },
    NeuralVoice {
        info: VoiceInfo {
            id: "kokoro-ru-dima",
            name: "Dima",
            language: "ru-RU",
            gender: Gender::Male,
            engine: "neural",
            description: "Russian studio natural voice, Dima (24 kHz, StyleTTS2).",
            license: KOKORO_RU_LICENSE,
            installed: false,
        },
        file: "dima.pt",
    },
    NeuralVoice {
        info: VoiceInfo {
            id: "kokoro-ru-masha",
            name: "Masha",
            language: "ru-RU",
            gender: Gender::Female,
            engine: "neural",
            description: "Russian studio natural voice, Masha (24 kHz, StyleTTS2).",
            license: KOKORO_RU_LICENSE,
            installed: false,
        },
        file: "masha.pt",
    },
];

pub fn find(id: &str) -> Option<&'static NeuralVoice> {
    VOICES.iter().chain(KOKORO_RU_VOICES.iter()).chain(PIPER_RU_VOICES.iter()).find(|v| v.info.id == id)
}

/// Returns the package ID responsible for downloading and managing the given neural voice ID.
pub fn package_for_voice(voice_id: &str) -> Option<&'static str> {
    if VOICES.iter().any(|v| v.info.id == voice_id) {
        Some(PACKAGE_ID)
    } else if KOKORO_RU_VOICES.iter().any(|v| v.info.id == voice_id) {
        Some(KOKORO_RU_PACKAGE_ID)
    } else if PIPER_RU_VOICES.iter().any(|v| v.info.id == voice_id) {
        Some(PIPER_RU_PACKAGE_ID)
    } else {
        None
    }
}

/// Checks whether voice `voice_id` is installed and ready for local inference in `models_dir`.
pub fn is_voice_installed(voice_id: &str, models_dir: &Path) -> bool {
    match package_for_voice(voice_id) {
        Some(PACKAGE_ID) => installed(models_dir) && cfg!(feature = "kokoro"),
        Some(KOKORO_RU_PACKAGE_ID) => kokoro_ru_installed(models_dir) && cfg!(feature = "kokoro"),
        Some(PIPER_RU_PACKAGE_ID) => piper_ru_installed(models_dir) && cfg!(feature = "piper"),
        _ => false,
    }
}

/// Total download size for Kokoro.
pub fn size() -> u64 {
    FILES.iter().map(|f| f.size).sum()
}

/// Returns the package directory path under `models_dir` for English Kokoro-82M voices.
pub fn package_dir(models_dir: &Path) -> PathBuf {
    models_dir.join(PACKAGE_ID)
}

/// Every file present with its pinned size (contents were verified when downloaded).
pub fn installed(models_dir: &Path) -> bool {
    let d = package_dir(models_dir);
    FILES.iter().all(|f| std::fs::metadata(d.join(f.name)).is_ok_and(|m| m.len() == f.size))
}

/// Bytes still to download for Kokoro.
pub fn missing_bytes(models_dir: &Path) -> u64 {
    let d = package_dir(models_dir);
    FILES.iter().filter(|f| !std::fs::metadata(d.join(f.name)).is_ok_and(|m| m.len() == f.size)).map(|f| f.size).sum()
}

/// Returns the package directory path under `models_dir` for Piper Russian voices.
pub fn piper_ru_package_dir(models_dir: &Path) -> PathBuf {
    models_dir.join(PIPER_RU_PACKAGE_ID)
}

/// Returns true when all required Piper Russian model and config files exist with correct size.
pub fn piper_ru_installed(models_dir: &Path) -> bool {
    let d = piper_ru_package_dir(models_dir);
    PIPER_RU_FILES.iter().all(|f| std::fs::metadata(d.join(f.name)).is_ok_and(|m| m.len() == f.size))
}

/// Total package size in bytes for Piper Russian models.
pub fn piper_ru_size() -> u64 {
    PIPER_RU_FILES.iter().map(|f| f.size).sum()
}

/// Remaining un-downloaded byte count for Piper Russian models.
pub fn piper_ru_missing_bytes(models_dir: &Path) -> u64 {
    let d = piper_ru_package_dir(models_dir);
    PIPER_RU_FILES.iter().filter(|f| !std::fs::metadata(d.join(f.name)).is_ok_and(|m| m.len() == f.size)).map(|f| f.size).sum()
}

/// Returns the package directory path under `models_dir` for Kokoro Russian models.
pub fn kokoro_ru_package_dir(models_dir: &Path) -> PathBuf {
    models_dir.join(KOKORO_RU_PACKAGE_ID)
}

/// Returns true when all required Kokoro Russian weights and voice packs exist with correct size.
pub fn kokoro_ru_installed(models_dir: &Path) -> bool {
    let d = kokoro_ru_package_dir(models_dir);
    KOKORO_RU_FILES.iter().all(|f| std::fs::metadata(d.join(f.name)).is_ok_and(|m| m.len() == f.size))
}

/// Total package size in bytes for Kokoro Russian models.
pub fn kokoro_ru_size() -> u64 {
    KOKORO_RU_FILES.iter().map(|f| f.size).sum()
}

/// Remaining un-downloaded byte count for Kokoro Russian models.
pub fn kokoro_ru_missing_bytes(models_dir: &Path) -> u64 {
    let d = kokoro_ru_package_dir(models_dir);
    KOKORO_RU_FILES.iter().filter(|f| !std::fs::metadata(d.join(f.name)).is_ok_and(|m| m.len() == f.size)).map(|f| f.size).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_pinned_and_voices_resolve() {
        for f in FILES {
            assert_eq!(f.sha256.len(), 64, "{}", f.name);
            assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(
                f.url.starts_with("https://")
                    && (f.url.contains("/f3ff3571791e39611d31c381e3a41a3af07b4987/") || f.url.contains("/74790861f652b15e4ac49015a90074ad62a27690/")),
                "{}",
                f.url
            );
            assert!(!f.name.contains('/'), "flat package layout: {}", f.name);
        }
        for v in VOICES {
            assert!(FILES.iter().any(|f| f.name == v.file), "{} has no file", v.info.id);
            assert_eq!(find(v.info.id).map(|x| x.file), Some(v.file));
        }
        assert!(size() > 330_000_000 && size() < 340_000_000, "{}", size());
        let dir = std::env::temp_dir().join(format!("filmcraft-tts-cat-{}", std::process::id()));
        assert!(!installed(&dir));
        assert_eq!(missing_bytes(&dir), size());
    }

    #[test]
    fn kokoro_ru_files_are_pinned_and_voices_resolve() {
        let dir = std::env::temp_dir().join(format!("filmcraft-tts-cat-kokoro-{}", std::process::id()));
        for f in KOKORO_RU_FILES {
            assert_eq!(f.sha256.len(), 64, "{}", f.name);
            assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(f.url.starts_with("https://"), "{}", f.url);
            assert!(!f.name.contains('/'), "flat package layout: {}", f.name);
        }
        for v in KOKORO_RU_VOICES {
            assert!(KOKORO_RU_FILES.iter().any(|f| f.name == v.file), "{} has no file", v.info.id);
            assert_eq!(find(v.info.id).map(|x| x.file), Some(v.file));
        }
        assert!(kokoro_ru_size() > 328_000_000 && kokoro_ru_size() < 330_000_000, "{}", kokoro_ru_size());
        assert!(!kokoro_ru_installed(&dir));
        assert_eq!(kokoro_ru_missing_bytes(&dir), kokoro_ru_size());
    }

    #[test]
    fn piper_ru_files_are_pinned_and_voices_resolve() {
        let dir = std::env::temp_dir().join(format!("filmcraft-tts-cat-piper-{}", std::process::id()));
        for f in PIPER_RU_FILES {
            assert_eq!(f.sha256.len(), 64, "{}", f.name);
            assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(f.url.starts_with("https://"), "{}", f.url);
            assert!(!f.name.contains('/'), "flat package layout: {}", f.name);
        }
        for v in PIPER_RU_VOICES {
            assert!(PIPER_RU_FILES.iter().any(|f| f.name == v.file), "{} has no file", v.info.id);
            assert_eq!(find(v.info.id).map(|x| x.file), Some(v.file));
        }
        assert!(piper_ru_size() > 120_000_000 && piper_ru_size() < 130_000_000, "{}", piper_ru_size());
        assert!(!piper_ru_installed(&dir));
        assert_eq!(piper_ru_missing_bytes(&dir), piper_ru_size());
        assert_eq!(package_for_voice("piper-dmitri"), Some(PIPER_RU_PACKAGE_ID));
        assert_eq!(package_for_voice("kokoro-heart"), Some(PACKAGE_ID));
        assert_eq!(package_for_voice("unknown"), None);
    }
}
