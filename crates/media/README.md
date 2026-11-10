# filmcraft-media

Media sources, still-image and image-sequence import, generated media, audio streams, and decoded
frame caching for FilmCraft.

## Openers

`Opener` is a shared closure (`Arc<dyn Fn(&str, Arc<[u8]>) -> Option<Result<SharedSource>> + Send + Sync>`),
so it can hold state. `OpenerFn` keeps the plain-fn shape and `opener_of` wraps one as an `Opener`.
Each host registers its own opener and owns its own format book. There is no process-wide list.

## Image-sequence filenames

`sequence::Numbered` identifies the last run of 1–18 ASCII frame-number digits in a filename's
stem when the extension is a recognized still-image format. `/` and `\` separate directory
components. A prefix may contain other UTF-8 characters, including accented letters, CJK and emoji:
`é0001.png`, `图0001.png` and `🎞️0001.png`. Parsing preserves the prefix, number padding and suffix
when generating sibling-frame paths. Sibling recognition compares the prefix's UTF-8 bytes exactly.
Stems without ASCII digits or with a final frame-number run longer than 18 characters are not numbered
image-sequence filenames.
