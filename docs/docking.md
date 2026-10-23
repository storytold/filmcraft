# Moving panels

Drag a panel by its title tab to another region of the editor. A blue highlight previews the
destination before release:

- The middle of a region adds the panel to that region's tabs.
- A region's left, right, top or bottom edge creates a separate region beside it.
- A tab header inserts the panel before or after the tab under the pointer.

For example, open Effects from Window, then drag its Effects tab into the middle of the top-right
region. Drop near an edge instead if Effects should have its own region. Resize regions with their
gutters. Tools and Audio Meters use the small grip at the top in place of a title tab.

Press Escape during a drag, or release outside the editor window without the Command/Ctrl modifier,
to cancel. Empty source regions collapse after a successful move.

The upstream shared docking system also supports floating panel groups within the editor. Hold
Command (macOS) or Ctrl while dragging to float a panel, or use its panel menu. Drag its tab back
onto a docked region to return it. Floating positions and sizes save with the workspace.

The Timeline's sequence tabs still reorder when dragged sideways along their own header. Drag a
sequence tab out of that header to move the Timeline panel, keeping its open sequences together.

To keep an arrangement across launches, use **Window → Workspaces → Save Changes to this Workspace**
or **Save as New Workspace**. **Reset to Saved Layout** restores the saved arrangement. Layout changes
do not modify media, timeline edits or the project's undo history.

Automation uses the existing docking command:
`window.dockPanel {"panel":"Effects","anchor":"Properties","zone":"center"}`.
Both panels must already be open, either docked or floating. Zones are `center`, `left`, `right`,
`top` and `bottom`; floating destinations accept tabs only. To insert before a destination tab,
pass its name as `before`. Moves use the shared validated docking transactions.
Panel title tabs register `panel.tab.<PanelKind>`; the current highlighted drop destination registers
`dock.drop.<PanelKind>.<placement>` during a drag.
