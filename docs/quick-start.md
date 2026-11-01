# Quick Start

This guide walks you through opening FilmCraft, importing media, building a
sequence, applying a basic colour correction, and exporting your first file.
Keyboard shortcuts use **Cmd** on macOS and **Ctrl** on Windows/Linux wherever
the two differ.

## 1. Open the application

```sh
cargo run --release -p filmcraft
```

FilmCraft opens with a built-in demo project so you can explore the interface
straight away.  To start from a blank project, pass `--empty`:

```sh
cargo run --release -p filmcraft -- --empty
```

A project is auto-saved as `filmcraft.fcp` in your home directory.  To open an
existing project file, pass its path:

```sh
cargo run --release -p filmcraft -- my-film.fcp
```

## 2. Import media

1. Choose **File > Import** (Cmd/Ctrl+I) or drag files directly onto the Project
   panel.
2. FilmCraft accepts MOV, MP4, MXF, WAV, AIFF and many other formats.  The
   import dialog lets you select multiple files at once.
3. Imported clips appear in the **Project** panel.  Click a clip to preview it in
   the Source Monitor.

## 3. Place clips on the timeline

1. In the Project panel, select one or more clips.
2. Drag them onto a video or audio track in the **Timeline**, or use
   **Clip > Insert** (,) / **Clip > Overwrite** (.) to edit at the playhead.
3. Trim clip edges by dragging the in/out points directly in the Timeline, or
   set them precisely in the Source Monitor with **I** (mark in) and **O**
   (mark out) then **F10** (overwrite to sequence).
4. Press **Space** to play back the sequence.  The playhead position is shown in
   the **Program Monitor**.

## 4. Basic colour correction

1. Select a clip on the Timeline.
2. Open the **Effects** panel and expand **Colour Correction**.
3. Drag **Lumetri Colour** (or the built-in **Basic Colour** effect) onto the
   clip, or double-click to apply it to the selected clip.
4. Adjust **Exposure**, **Contrast**, **Highlights** and **Shadows** in the
   Effect Controls panel.  Changes are non-destructive and can be reset at any
   time via the **Reset** button next to each parameter.

## 5. Export

1. Choose **File > Export** (Cmd/Ctrl+M) to open the Export dialog.
2. Select a preset (e.g. *H.264 — 1080p 29.97*) or configure the format,
   codec and destination manually.
3. Click **Export** to render the sequence to disk.  Progress is shown in the
   status bar.  FilmCraft writes the output atomically: the destination file
   appears only after the render completes successfully.

For more detail on any topic, see the docs directory:

- [architecture.md](architecture.md) — crate structure and layering rules
- [keyboard.md](keyboard.md) — full keyboard shortcut reference
- [testing.md](testing.md) — running the test suite
- [contributing.md](contributing.md) — how to contribute code
