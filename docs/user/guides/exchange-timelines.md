---
title: Move a timeline to or from another editor
description: Import and export timelines as Final Cut Pro XML, FCPXML, OpenTimelineIO, EDL, AAF and OMF, for example to bring a Premiere sequence into FilmCraft.
order: 50
verified: 0.7.2
---

To bring a sequence in, choose **File ▸ Import…** and pick the exchange file. To send one out, open the sequence and choose a format under **File ▸ Export**.

## Bring a Premiere sequence into FilmCraft

FilmCraft cannot open Premiere `.prproj` project files yet. Export the sequence from Premiere as Final Cut Pro 7 XML, AAF or OpenTimelineIO, then:

1. In FilmCraft, choose **File ▸ Import…**.
2. Pick the exported file.
3. Find the imported sequences in the Project panel.

Import adds the file's bins, media and sequences to your project in one undoable step, and links each media file it finds on disk.

## Send a sequence out

Open the sequence, then choose one of these under **File ▸ Export**:

| Menu item                  | Format                  | Use it for                                                 |
|----------------------------|-------------------------|------------------------------------------------------------|
| **Final Cut Pro XML…**     | Final Cut Pro 7 XML     | The format Premiere and DaVinci Resolve exchange           |
| **FCPXML…**                | FCPXML 1.9 to 1.11      | Spine, connected clips, transitions, retiming              |
| **OpenTimelineIO…**        | OpenTimelineIO          | Open interchange, keeps FilmCraft details                  |
| **EDL…**                   | CMX 3600 EDL            | Old but common; one EDL per track                          |
| **AAF…**                   | AAF (Edit Protocol)     | Avid Media Composer and Pro Tools                          |
| **OMF…**                   | OMF 2.0                 | Audio post handoff, with crossfades and gain               |

Marker review notes go out through **Markers ▸ Export Markers as CSV…**.

## Limits

Each format carries different things. Final Cut Pro 7 XML carries nested sequences, transitions, generators, Motion, Opacity, Time Remap and audio levels, with keyframes. AAF carries video and audio tracks, dissolves and dips, clip volume with keyframes, markers and source timecode. Check the result in the other editor before you rely on it.
