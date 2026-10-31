---
title: Recover unsaved work
description: Bring back unsaved changes after FilmCraft quit, crashed or lost power, using the Recover Unsaved Changes dialog.
order: 60
verified: 0.8.0
---

If FilmCraft was closed or quit with unsaved changes, it offers to bring them back the next time it starts. Click **Recover**.

## Steps

1. Start FilmCraft. If unsaved changes were found, the **Recover Unsaved Changes** dialog opens. It says whether FilmCraft was closed or quit unexpectedly, names the project and shows the time of the saved state.
2. If several sessions have unsaved changes, the dialog lists them. Select the one you want.
3. Click **Recover**. FilmCraft opens the project with those changes.
4. Choose **File ▸ Save** to keep them.

**Discard** deletes the unsaved changes of the selected session. **Not Now** closes the dialog and keeps them.

## If you clicked Not Now

Choose **File ▸ Recover Unsaved Changes…**. The menu item works only while there are unsaved changes to recover. Otherwise FilmCraft reports that there are none.

## How it works

While **Keep a recovery copy of unsaved changes** is on (the default), FilmCraft copies unsaved changes in the background. It writes the copy after about a second without edits. While you keep editing, it still writes one at least every 5 seconds; **Update it at least every** changes that. Recovery brings back the last copy that was written, so the most recent edits before a crash or power cut may be missing. With the setting off, there is nothing to recover.

Saves are atomic, and auto-save keeps a rolling set of versions. These settings are under **Edit ▸ Preferences ▸ Auto Save…** (on macOS, **FilmCraft ▸ Settings ▸ Auto Save**), in the **Crash Recovery** group.
