# macOS DMG window

What Finder shows when the DMG opens: a 660 × 400 pt background with the app icon and the
`Applications` link side by side. `package.sh` copies these files into the image; nothing here is
generated at build time, so the DMG builds with a plain `hdiutil create -srcfolder` (no Finder
scripting on CI, nothing extra installed in the signing job).

| File | What |
|---|---|
| `background.svg` | Source of the background: the app icon (`assets/app-icon/filmcraft.svg`, linked, not copied) cropped as a cover on the FilmCraft colour field (`#8b5cf6`), Ink and Paper. Its text (Inter, JetBrains Mono) is outlined, so rendering it needs no fonts. No paper grain: noise doesn't compress, and this keeps `background.tiff` small. |
| `background.tiff` | The background at 1x (660 × 400 px, 72 dpi) and 2x (1320 × 800 px, 144 dpi) in one HiDPI TIFF (16-colour palette, Deflate, sRGB). Goes to `.background/background.tiff`. |
| `dmg-layout.DS_Store` | Finder's view settings for the volume: window size, icon size 128, FilmCraft.app at (326, 205), `Applications` at (574, 205), and the background. Goes to `.DS_Store` in the image; named so it isn't mistaken for (or ignored like) a Finder-generated `.DS_Store`. |
| `generate.py` | Writes `background.tiff` and `dmg-layout.DS_Store` from the SVG and the layout above. |

## The volume name has no version

The mounted volume is called `FilmCraft`, not `FilmCraft <version>`. The layout points at the background
through an alias that includes the volume name, and Finder resolves it by that name: with a
versioned name the window keeps its size and icon positions but shows no background (tested).
The DMG file name (`filmcraft-<version>-macos-<arch>.dmg`) still carries the version.

## `hdiutil create`, not `makehybrid`

`hdiutil makehybrid -hfs` gives every file and folder on the volume a `com.apple.FinderInfo` (a
Finder location of -1,-1), even when the staged files have none. `codesign --verify --strict`
rejects it on the app in the image and on every copy installed from it ("resource fork, Finder
information, or similar detritus not allowed", #882), though Gatekeeper accepts the app.
`hdiutil create -srcfolder` writes the files as they are. `package.sh` checks the app in the
finished image (`check_dmg_app` in `../dmg.sh`), and `../dmg-test.sh` tests both ways.

The one thing `create` can't do is `makehybrid -hfs-openfolder`, which listed the volume's root as
its first open folder. Opening the window doesn't depend on it: double-clicking a read-only disk
image opens its volume in the Finder (hdiutil(1), `auto-open-ro-root`). The window's background,
icon size and positions come from `.DS_Store`, as before.

## Rules

- **Finder draws the icon labels in black in light and dark mode** when a window has a background,
  so the area under both icons stays light (Paper).
- **Nothing goes inside the icon boxes:** artwork keeps 10 pt clear of each 128 pt icon box and of
  the label strip under it.

## Regenerate

Edit `background.svg` (or the layout constants in `generate.py`), then run, on any OS:

```sh
pip install 'pillow>=12' ds_store==1.3.3 mac_alias==2.2.3
python3 packaging/macos/dmg/generate.py   # needs resvg on PATH
```

Both outputs are byte-for-byte reproducible and carry nothing from the machine that ran it: the
SVG renders without any fonts, and `dmg-layout.DS_Store` is written from scratch, its background
alias holding only the volume name and `/.background/background.tiff`. If you add text to the SVG,
outline it (`usvg` from resvg converts text to paths). The window is 660 × 432 with Finder's 32 pt
title bar; the content area is 660 × 400.

This follows VectorCraft's `packaging/macos/dmg/` (storytold/vectorcraft#493).
