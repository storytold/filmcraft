# filmcraft-vision

Optional CPU text/image embeddings for local media search. Enable `clip` to load LAION/OpenCLIP
ViT-B/32 through Candle 0.9 and the Hugging Face tokenizer. The crate performs no networking and
never discovers or downloads weights. Inputs are RGBA frames and English text; outputs are
normalized 512-dimensional vectors. Empty/non-finite/wrong-sized vectors and malformed/oversized
frames return errors.

Preprocessing follows CLIP's shortest-side resize to 224, centre crop, RGB mean/std normalization,
and BOS/EOS tokenizer. Query length is bounded to 1024 bytes and 77 model tokens. Frames are
limited to 16,777,216 pixels; extreme aspect ratios are rejected. CPU only; no Python, LibTorch,
CUDA or system inference library is linked. `image` is used only for in-memory resizing in the
product; the engine decodes source media through Filmcraft.

The engine owns pinned downloads, checksum verification, background scheduling and disk caches.
See [visual-search.md](../../docs/visual-search.md). LAION's MIT model licence:
https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K/blob/1a25a446712ba5ee05982a381eed697ef9b435cf/README.md. Model card and limitations:
https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K.

The explicit ignored `clip_model` test loads real weights and a local image manifest
`[{"path":"/absolute/image.jpg","query":"visible content"},…]`. Set `FILMCRAFT_CLIP_DIR` and
`FILMCRAFT_VISUAL_FIXTURES`, then run `cargo test -p filmcraft-vision --features clip --test
clip_model -- --ignored --nocapture`. Tests do not download weights or fixtures and do not claim
that a small retrieval fixture measures general search accuracy.

The internal encoder adapts Candle 0.9.2 under Apache-2.0 because its published CLIP API hardcodes QuickGELU for text. LAION requires standard GELU in both encoders. Source attribution and licence are retained in `src/encoder/`.
