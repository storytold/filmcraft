# Script window settings inventory

The Script toolbar has a gear menu for settings that are implemented today. This inventory tracks the other requested controls without presenting them as working features.

Shortening either candle endpoint changes only its visible coverage. Script marks, per-row colors, and off-screen passages on hidden lines remain stored with their original line IDs; marks also retain their IDs and exact source times. Expanding the candle reveals them again. Hidden marks have no canvas target and do not anchor visible-row cue interpolation. Deleting a logical line removes its annotations even when that line is hidden. No Script project schema change is needed.

Backspace or forward Delete removes the selected Script object: a mark, a candle, or a setup selected by its thumbnail. Deleting a thumbnail removes that entire setup with all its candles and marks. Deleting the last candle also removes its empty setup. Script text and Project media stay intact, and Undo restores the deleted objects. With no object selected these keys do nothing; in a text field they retain normal text-editing behavior.

Rows can be selected across the entire canvas width, beyond both text margins. Shift-click extends the selected range. Thumbnails, candles, and marks keep their own click actions; clicking elsewhere on a row clears those active objects and selects the row. Clicking below the rows clears the row selection.

Clips dragged from the Project panel can be dropped across the entire Script canvas width, including open space between take columns. When rows are selected, dropping anywhere on the canvas creates a new setup covering that entire selection, regardless of the row or floating object under the pointer. The preview remains on the selected range. Without a row selection, the hovered line determines coverage, a drop below the last line uses the last line, a thumbnail drop adds to its setup, and a candle drop rebinds its source with confirmation when it has marks.

| Control | FilmCraft state |
| --- | --- |
| Font and size | Text-size control in the gear menu; font-family choice pending. |
| Left margin, text width | Gear controls and draggable ruler markers. |
| Word Wrap | Always on; toggle pending. |
| Take Coloring | Set Color toolbar button and per-row candle colors; a separate default-color setting pending. |
| Pre-Roll | Pending; custom Script play/record remains deferred. |
| Show Frames | Pending. |
| Show All Takes | All takes are shown; filtering toggle pending. |
| Show Line Number | Gear toggle. |
| Interpolate Position | Double-clicking a candle cues Source at its start when unmarked, or interpolates between surrounding script marks by row. Continuous playback behavior remains pending. |
| Hold Slates Onscreen | Pending; requires scroll behavior design. |
| Script Window Background / White Background | Uses FilmCraft theme; white override pending. |
| Show Row Colors | Gear toggle. |

Possible context actions include Play, Delete, Find in Project, Add Script Mark, Apply/Clear Color to Range, Set Font, Script Settings, Show Script in Project, and Hide Sidebar. FilmCraft currently exposes mark, color-range, and Find in Project actions through its Script and Project controls. The other context actions are not implemented by this inventory.
