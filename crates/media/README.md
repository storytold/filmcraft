# filmcraft-media

Media sources, still-image and image-sequence import, generated media, audio streams, and decoded
frame caching for FilmCraft.

## Image-sequence filenames

`sequence::Numbered` identifies the last run of 1–18 ASCII frame-number digits in a filename's
stem when the extension is a recognized still-image format. `/` and `\` separate directory
components. A prefix may contain other UTF-8 characters, including accented letters, CJK and emoji:
`é0001.png`, `图0001.png` and `🎞️0001.png`. Parsing preserves the prefix, number padding and suffix
when generating sibling-frame paths. Sibling recognition compares the prefix's UTF-8 bytes exactly.
Stems without ASCII digits or with a final frame-number run longer than 18 characters are not numbered
image-sequence filenames.

## Media capabilities

The capabilities module defines serializable CPU/GPU inventory and H.264/HEVC/AV1
implementation records. Available means an implementation candidate; exact configuration
and session checks are still required. Formats list a confirmed V1 subset.

Unknown limits and HDR/control sets use None. A known empty set uses Some(empty).
Read untrusted snapshots with decode_snapshot: it enforces the 8 MiB input limit and
validates schema, counts, identifiers, formats and adapter associations. Do not call
serde_json::from_slice directly on unbounded input. Write transport data with
encode_snapshot, which validates the model and bounds output.

This module performs no OS discovery, hardware probing or media encoding/decoding.
