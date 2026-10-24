# Script window settings inventory

The Script toolbar has a gear menu for settings that are implemented today. This inventory tracks the other requested controls without presenting them as working features.

Toolbar controls wrap when the panel is narrow. Scene/Page captions stay with their menus, the **?** help button stays visible and aligned with the first row, and search sits below the wrapped controls. The **?** button opens the same getting-started guide shown before a script is created or imported.

Paste plain text with Cmd-V/Ctrl-V or Edit → Paste. With no script open, this creates a script. Otherwise it inserts after the selected row range, or appends when no rows are selected. Inside a row editor, paste replaces the character selection at the caret; each hard line break adds one logical row and shifts later rows down. Windows CRLF counts as one break. Existing row IDs, labels, candles and marks remain attached, and one Undo removes the paste while retaining text typed beforehand. Clicking outside the row editor, including another row's right margin, commits valid changes and closes the text box even if it has already lost keyboard focus. An unchanged blur adds no Undo step.

Shortening either candle endpoint changes only its visible coverage. Script marks, per-row colors, and off-screen passages on hidden lines remain stored with their original line IDs; marks also retain their IDs and exact source times. Expanding the candle reveals them again. Hidden marks have no canvas target and do not anchor visible-row cue interpolation. Deleting a logical line removes its annotations even when that line is hidden. No Script project schema change is needed.

Backspace or forward Delete removes the selected Script object: a mark, a candle, or a setup selected by its thumbnail. Deleting a thumbnail removes that entire setup with all its candles and marks. Deleting the last candle also removes its empty setup. Script text and Project media stay intact, and Undo restores the deleted objects. With no object selected these keys do nothing; in a text field they retain normal text-editing behavior.

Rows can be selected across the entire canvas width, beyond both text margins. Shift-click extends the selected range. Thumbnails, candles, and marks keep their own click actions; clicking elsewhere on a row clears those active objects and selects the row. Clicking below the rows clears the row selection.

Clips dragged from the Project panel can be dropped across the entire Script canvas width, including open space between take columns. Selected rows determine inclusive candle coverage regardless of the pointer row. New setups stay at the pointer's horizontal position, including when scrolled or when their thumbnails overlap another setup. A drop directly on an online thumbnail adds a candle to that setup, using selected rows or its last candle's coverage; selected-row drops outside the thumbnail create a separate setup. Without selected rows, the hovered line determines a new setup's coverage and a drop below the last line uses the last line.

Offline or deleted-source candles and thumbnails accept recovery drops before new-setup creation. Their menus also offer **Rebind to Selected Project Clip**. Recovery reconnects matching candles across scripts using the original Project source ID or retained footage fingerprint, never a filename match, in one Undo step. Matching footage preserves marks and styling; incompatible footage or a source span excluding existing marks requires review before a whole-batch replacement can clear marks. Without a retained source ID or fingerprint, only the targeted candle can be identified safely. Ordinary candle rebinding without a row selection retains its review safeguards.

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
