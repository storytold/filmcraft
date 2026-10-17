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
directories are refused. `sequence` optionally chooses the active sequence by name; all sequences
are imported. Without it, “Sequence 01” wins, then the sequence with the most video items.

The destination contains the unchanged `source.prproj`, `provenance.json`, and (copy mode) a
`media/` directory with full SHA-256 named files. Link mode uses original absolute paths without
writing to those files. The project stores the original source paths, hashes, source tree and
warnings independently of timeline edits. Undo does not delete the saved source snapshot.

Files with workflow provenance use the distinct `filmcraft.project.workflow` format tag and
workflow version 1. Once saved with provenance, a file requires a build that understands this
tag. Older FilmCraft versions reject it, preventing them from silently discarding provenance
on save. Ordinary projects still use the upstream tag and schema. Undoing the import can remove
its provenance record; save/reopen retains records that remain in the project.
Copying just the `.fcproj` does not include its linked media or source snapshot. They must remain
available at their recorded locations; automatic portable-project conversion is outside this importer.

Supported: video/audio tracks, mute state, exact tick ranges, constant positive speed, nested
sequences, still media, caption text, plain text graphics, static Motion scale/position/rotation,
and clip-edge fades following the Python importer's transition mapping. Unknown effects remain
in the original snapshot and are reported, rather than silently recreated with guessed parameters.
Fonts/styles and transition appearance are not guaranteed to match Premiere. Missing, ambiguous
or unreadable media remains visible as offline placeholders. A failure to open/read a media
file for hashing reports its path and the OS error, leaves its hash/size unknown, and continues
the import in both link and copy modes. It does not attempt to change permissions or copy an
unreadable source. Destination write failures and failed copy-integrity checks still abort.

Each sequence reads `Sequence/TrackGroups/TrackGroup/Second@ObjectRef` →
`VideoTrackGroup/TrackGroup/FrameRate` as ticks per frame (254,016,000,000 ticks/second).
The nearest rate from FilmCraft's common sequence rates is selected using integer rational
comparisons, including exact 24000/1001, 30000/1001 and 60000/1001. Audio sample rates and
source-media rates do not set the sequence rate. `frameRate` explicitly overrides every sequence;
only a sequence without its own rate uses the warned 24 fps fallback. Invalid/nonpositive tick
values are rejected. Integer timeline ticks are preserved in every case. Media lookup
follows the reference's clip-title matching; duplicate titles with different paths are refused
as ambiguous. Only timeline-referenced media is imported; unused bin media and bin organization
are not reconstructed. Reverse/time-remapped clips, native effect parameters and unsupported caption
encodings need additional fixtures before claiming fidelity. No rendered-preview substitution
is performed.

Projects containing only settings and no `Sequence` objects currently report an empty-document
error. They do not create an imported project or destination; settings-only import is unsupported.

Input is bounded to 64 MiB compressed/expanded XML, 500,000 XML nodes, 256 sequences, 256 tracks
per kind and 100,000 timeline items. A traversal budget also accounts for repeated references and text size; diagnostics are capped at 1,024 entries plus a truncation notice. DTD/entity expansion, duplicate IDs, invalid times/speeds,
cyclic nesting and nesting deeper than 64 levels produce errors. A failed import removes only
its newly created destination and leaves the editor project unchanged.
