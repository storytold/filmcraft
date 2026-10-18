//! Pinned, explicitly MIT-licensed LAION/OpenCLIP catalogue. Never bundled.
use filmcraft_speech::models::ModelFile;
pub const MODEL_REVISION: &str = "1a25a446712ba5ee05982a381eed697ef9b435cf";
pub const FILES: &[ModelFile] = &[
    ModelFile {
        name: "config.json",
        url: "https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K/resolve/1a25a446712ba5ee05982a381eed697ef9b435cf/config.json",
        sha256: "1284cbff35169abb23a1c5525a8b0f543c7bd191d4b9aed63880c1571bc4191c",
        size: 4355,
    },
    ModelFile {
        name: "tokenizer.json",
        url: "https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K/resolve/1a25a446712ba5ee05982a381eed697ef9b435cf/tokenizer.json",
        sha256: "b556ac8c99757ffb677208af34bc8c6721572114111a6e0aaf5fa69ff0b8d842",
        size: 2224041,
    },
    ModelFile {
        name: "preprocessor_config.json",
        url: "https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K/resolve/1a25a446712ba5ee05982a381eed697ef9b435cf/preprocessor_config.json",
        sha256: "910e70b3956ac9879ebc90b22fb3bc8a75b6a0677814500101a4c072bd7857bd",
        size: 316,
    },
    ModelFile {
        name: "pytorch_model.bin",
        url: "https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K/resolve/1a25a446712ba5ee05982a381eed697ef9b435cf/pytorch_model.bin",
        sha256: "8693edd165efde117185f3e771f1b09bc01f9c19027be4d6d4424bb38f605e4a",
        size: 605239073,
    },
    ModelFile {
        name: "README.md",
        url: "https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K/resolve/1a25a446712ba5ee05982a381eed697ef9b435cf/README.md",
        sha256: "d0600101ba66849bd84eeae14e0b1ccd56cd962ea94aec5d60c415d9ce4e5f28",
        size: 7464,
    },
];
