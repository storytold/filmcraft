# Premiere import

`premiere.import` reads gzip or plain XML based on the LLM Video Editor Python parser and
structural observations from user-owned project XML. No Adobe application code, proprietary
documentation or assets were consulted or copied. The committed fixtures are hand-written
with placeholder names and paths; private projects and media are not included.
New code is MIT OR Apache-2.0.

```sh
# Substitute your project and NEW destination. A rendered preview is not required.
filmcraft-cli --save-as /path/to/episode.fcproj exec premiere.import \
  '{"path":"/path/to/source.prproj","destination":"/path/to/episode-media","mediaMode":"link"}'
```

The same JSON command works through MCP `command_run` or the desktop control channel.
File → Import and `filmcraft-cli import source.prproj` also dispatch to this importer. Their
default is linked media and a new `source.filmcraft-import-N` directory beside the source project.
Use the explicit command for a custom destination, copied media or frame rate. Existing destination
directories are refused. `sequence` optionally chooses the active sequence by ObjectUID or unique
name; an ambiguous requested name requires its ObjectUID. All sequences
are imported. Without it, the sequence with the most video items wins; “Sequence 01” is only
a tie-break, followed by the latest video end time.

The destination contains the unchanged `source.prproj`, `provenance.json`, and (copy mode) a
`media/` directory with full SHA-256 named files. Link mode uses original absolute paths without
writing to those files. Linked imports probe media and retain the existing size/head/tail relink
fingerprint in both the media item and immutable provenance (at most 2 MiB read by the native
host), but defer full-file SHA-256. A matching fingerprint is not proof that every byte is unchanged.
Their provenance
`sha256` is null, even when the media is online; `bytes` records its size when available.
The source project is still hashed and snapshotted immediately. Copied media is hashed and verified
synchronously. The project stores the original source paths, available import-time hashes, source
tree and warnings independently of timeline edits. Undo does not delete the saved source snapshot.

For an explicit, potentially expensive inspection of current media, use:

```sh
filmcraft-cli --project /path/to/episode.fcproj exec premiere.hashMedia '{"import":0}'
```

`import` is the zero-based provenance record index. This read-only command repeats document-path,
extension and opener checks before hashing each current imported path. It returns SHA-256/size
or a per-file error; `matchesImport` compares against an existing import-time hash, or is null
when none was recorded. A later hash cannot prove what bytes existed at import time. The command
does not backfill provenance, rewrite `provenance.json`, save the project or add an undo step.
It runs synchronously only when explicitly requested; it is not a background job or a promise
that the remaining probe/copy work cannot block the UI.

Files with workflow provenance use the distinct `filmcraft.project.workflow` format tag and
workflow version 1. Once saved with provenance, a file requires a build that understands this
tag. Older FilmCraft versions reject it, preventing them from silently discarding provenance
on save. Ordinary projects still use the upstream tag and schema. Undoing the import can remove
its provenance record; save/reopen retains records that remain in the project.
Copying just the `.fcproj` does not include its linked media or source snapshot. They must remain
available at their recorded locations; automatic portable-project conversion is outside this importer.

Supported: video/audio tracks (track `IsMuted` only; the track-fader mute checkbox is not read),
exact tick ranges, constant positive speed, nested
sequences, still media, caption text, plain text graphics, static Motion scale/position/rotation,
and clip-edge fades following the Python importer's transition mapping. Unknown effects remain
in the original snapshot and are reported once per distinct effect name with an occurrence count,
rather than silently recreated with guessed parameters. Fader-muted audio imports audible unless
the track is also muted through `IsMuted`.
Fonts/styles and transition appearance are not guaranteed to match Premiere. Missing, ambiguous
or unreadable media remains visible as offline placeholders. Document references pass the existing
document-path policy before canonicalization. Both the reference and resolved file must have a
recognized media extension, and the registered media opener must accept the file before it is
hashed or copied. Non-media, refused paths and failed probes stay offline with only their original
reference recorded, no hash/size, and no copied file. Probing may read media headers or, for formats
without a streaming opener, the file itself; a recognized extension alone is insufficient.
A failure to open/read an accepted media file reports its path and the error and continues
the import in both link and copy modes. It does not attempt to change permissions or copy an
unreadable source. Destination write failures and failed copy-integrity checks still abort.

Each sequence reads `Sequence/TrackGroups/TrackGroup/Second@ObjectRef` →
`VideoTrackGroup/TrackGroup/FrameRate` as ticks per frame (254,016,000,000 ticks/second).
The nearest rate from FilmCraft's common sequence rates is selected using integer rational
comparisons, including exact 24000/1001, 30000/1001 and 60000/1001. Audio sample rates and
source-media rates do not set the sequence rate. `frameRate` explicitly overrides every sequence;
only a sequence without its own rate uses the warned 24 fps fallback. Invalid/nonpositive tick
values are rejected. Integer timeline ticks are preserved in every case. Media and nests follow
`SubClip/Clip` → `Clip/Source` → `VideoMediaSource` / `AudioMediaSource` or
`VideoSequenceSource` / `AudioSequenceSource` → `Media` / `Sequence`, resolving `ObjectRef` and
`ObjectURef` through the document's ID maps. Sequences are keyed by ObjectUID. Clip labels and
duplicate sequence/media display names do not affect a resolved reference. If the chain cannot
be resolved, an unambiguous title match is used with a warning; ambiguous fallbacks stay offline.
Only timeline-referenced media is imported; unused bin media and bin organization
are not reconstructed. Reverse/time-remapped clips, native effect parameters and unsupported caption
encodings need additional fixtures before claiming fidelity. No rendered-preview substitution
is performed.

Static Motion uses `MatchName` (`AE.ADBE Motion` / `ADBE Motion`) and ParameterID 1 (Position),
2 (Scale), and 5 (Rotation); localized display/parameter names are diagnostic text only. Values
come from `CurrentValue` or the static `StartKeyframe` record. Animated parameters are reported
and retained in provenance; their animation is not reconstructed. Other Motion parameter IDs
are not mapped. These fixes address the name/reference defects discussed in
[issue #978](https://github.com/storytold/filmcraft/issues/978#issuecomment-6254913542);
they do not add track-fader mute or change audio gain conversion.

Text graphics match the text component's `MatchName` and Source Text ParameterID 1, then decode
the static base64 `StartKeyframeValue` using the bounded length-prefixed UTF-8 envelope heuristic
also used for captions. This is based on structural observations of user-owned XML, not a complete
text-format specification. `InstanceName` is a layer label and is never substituted for visible text.
Missing, unreadable or animated Source Text is reported and left unresolved; fonts, styling and
text animation are not reconstructed.

Projects containing only settings and no `Sequence` objects currently report an empty-document
error. They do not create an imported project or destination; settings-only import is unsupported.

Input is bounded to 64 MiB compressed/expanded XML, 500,000 XML nodes, 256 sequences, 256 tracks
per kind and 100,000 timeline items. A traversal budget also accounts for repeated references and
text size; diagnostics are capped at 1,024 entries plus a truncation notice. Clips with nonpositive
timeline duration, non-finite speed or speed outside 0.001–1000, or inverted/unsupported source
ranges are skipped with a warning naming the clip. Other clips keep their original timeline
positions; skipped clips can leave gaps. A transition whose end precedes its start is skipped with
a warning, preserving its clip and any valid transition. DTD/entity expansion, duplicate IDs, negative/non-integer
timeline or source ticks, non-numeric speeds, cyclic nesting and nesting deeper than 64 levels
still produce errors. The referenced graph is validated before unsupported clips are removed,
so skipping a clip does not bypass the corruption or resource-limit checks. A failed import removes only
its newly created destination and leaves the editor project unchanged.

Open workflow issues: probing, copying and explicitly requested hashing are synchronous and can
block the caller on large inputs. Default linked import avoids the full-file SHA-256 pass;
formats without streaming openers may still be read in full for probing. Imports without an
explicit `destination` write beside the source project; the destination-prompt policy and
background-job dispatch remain follow-up work. These checks do
not ask for consent for each valid media reference, and do not verify rendered appearance.

## Regression and private-corpus checks

```sh
cargo test --locked -p filmcraft-engine -p filmcraft-interchange --lib premiere
FILMCRAFT_PREMIERE_CORPUS=/path/to/local-manifest.json \
  cargo test --locked -p filmcraft-engine -p filmcraft-interchange --lib premiere_private_corpus -- --ignored --nocapture
```

The opt-in corpus manifest is a local JSON array of `{"source":"/path/to/project.prproj",
"maxOffline":0}` entries; set each offline ceiling to its recorded baseline. An optional
`"frameRate":{"num":25,"den":1}` checks that every imported sequence has that expected rate;
it does not override the file's rate. When omitted, rates are recorded without an assertion.
The test copies
projects under `target/premiere-corpus/`, imports with linked media through a headless engine
session without registering platform hardware, verifies source/project hashes and offline counts,
and saves/reopens each result. Media SHA-256 is explicitly inspected before and after save/reopen;
linked provenance must remain unhashed throughout. Its private result JSON stays under `target/`. Only hand-written
synthetic XML is committed in the regression tests; never add corpus projects or media paths.
