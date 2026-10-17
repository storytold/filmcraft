# Premiere import

`premiere.import` reads gzip or plain XML using the object graph documented by the local
LLM Video Editor Python parser. No Adobe code or assets are used. New code is MIT OR Apache-2.0.

```sh
# Substitute your project and NEW destination. A rendered preview is not required.
filmcraft-cli --save-as /path/to/episode.fcproj exec premiere.import \
  '{"path":"/path/to/source.prproj","destination":"/path/to/episode-media","mediaMode":"link","frameRate":{"num":30000,"den":1001}}'
```

The same JSON command works through MCP `command_run` or the desktop control channel.
File → Import and `filmcraft-cli import source.prproj` also dispatch to this importer. Their
default is linked media and a new `source.filmcraft-import-N` directory beside the source project.
Use the explicit command for a custom destination, copied media or frame rate. Existing destination
directories are refused. `sequence` optionally chooses the active sequence by name; all sequences
are imported. Without it, “Sequence 01” wins, then the sequence with the most video items.

The destination contains the unchanged `source.prproj`, `provenance.json`, and (copy mode) a
`media/` directory with full SHA-256 named files. Link mode uses original absolute paths without
writing to those files. The project stores the original source paths, hashes, source tree and
warnings independently of timeline edits. Undo does not delete the saved source snapshot.

Files with workflow provenance use the distinct `filmcraft.project.workflow` format tag and
workflow version 1. Unmodified upstream FilmCraft rejects this tag, preventing silent provenance
loss. Ordinary projects still use the upstream tag and schema. Save/reopen retains the records.
Copying just the `.fcproj` is not a portable-project conversion; that is a separate port item.

Supported: video/audio tracks, mute state, exact tick ranges, constant positive speed, nested
sequences, still media, caption text, plain text graphics, static Motion scale/position/rotation,
and clip-edge fades following the Python importer's transition mapping. Unknown effects remain
in the original snapshot and are reported, rather than silently recreated with guessed parameters.
Fonts/styles and transition appearance are not guaranteed to match Premiere. Missing or ambiguous
media remains visible as offline placeholders.

The Python reference does not extract frame rate. This importer uses a warned 24 fps fallback
unless `frameRate` is supplied; integer timeline ticks are preserved either way. Media lookup
follows the reference's clip-title matching; duplicate titles with different paths are refused
as ambiguous. Reverse/time-remapped clips, native effect parameters and unsupported caption
encodings need additional fixtures before claiming fidelity. No rendered-preview substitution
is performed.

Input is bounded to 64 MiB compressed/expanded XML, 500,000 XML nodes, 256 sequences, 256 tracks
per kind and 100,000 timeline items. A traversal budget also accounts for repeated references and text size; diagnostics are capped at 1,024 entries plus a truncation notice. DTD/entity expansion, duplicate IDs, invalid times/speeds,
cyclic nesting and nesting deeper than 64 levels produce errors. A failed import removes only
its newly created destination and leaves the editor project unchanged.
