//! The presence projection (CraftHub spec §6.1, §18.4 row 8): what FilmCraft's live UI state
//! looks like on the wire, and what a peer's state renders as here. Presence is ephemeral — it
//! is never stored, never journalled, never saved (§2.7's rule, kept verbatim in v3).
//!
//! FilmCraft's row in the per-app projection table is the **timeline row**: the playhead and the
//! track range being edited, beside the shared cursor/selection/viewport fields.
//!
//! ## The playhead, and where its fps resolution lives
//!
//! The playhead rides [`UiState::playhead`] → [`PresenceMsg::playhead`] as **sequence ticks** —
//! the document's canonical time unit (`TICKS_PER_SECOND`), independent of any frame rate. A
//! peer resolves ticks to a frame number with the *sequence's own* frame rate, which is document
//! data — the `settings` property of the sequence item (a synced property, so every peer already
//! has the number) — never a presence field: presence carries where the playhead *is*, the
//! document says what a frame *is*. The active sequence rides the `page` field (its item id);
//! a playhead without one is meaningless, so the projection declines to speak.
//!
//! ## The track range
//!
//! [`UiState::track_range`] carries the `(track index, start, end)` a peer is dragging or
//! trimming — the "someone is working here" hint that pairs with the region locks (§6.7): the
//! lock is the *right* to edit, the range is the live *sight* of the edit in progress. FilmCraft
//! has no generation jobs and no live canvas text caret — those fields belong to the siblings
//! (ArtCraft, FigCraft) and stay `None` here.

use craft_collab::adapter::UiState;

/// Everything the FilmCraft UI needs to render one peer: the projection is a two-way street —
/// the same fields go out and come back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeerView {
    /// The peer's cursor in the active panel's coordinates.
    pub cursor: Option<(f64, f64)>,
    /// The sequence the peer is editing (the Timeline tab; the playhead's frame of reference).
    pub page: Option<u64>,
    /// The peer's selection (outline highlights, never edit locks — §6.7 does locks).
    pub selection: Vec<craft_collab::ids::NodeId>,
    /// The peer's viewport rectangle, offered to the follow mode.
    pub viewport: Option<(f64, f64, f64, f64)>,
    /// The peer's playhead, in sequence ticks (resolve to frames with the sequence's `settings`).
    pub playhead: Option<u64>,
    /// The track range the peer is editing: (track index, start tick, end tick).
    pub track_range: Option<(u32, u64, u64)>,
}

impl PeerView {
    /// Project FilmCraft's UI state into the fields the hub's `UiState` carries.
    ///
    /// The selection is bounded (§2.12: at most 10,000 ids) by the core's sanitizer, not here —
    /// the app reports what it truly has and the hub enforces the budget.
    #[must_use]
    pub fn from_ui(ui: &UiState) -> Self {
        Self { cursor: ui.cursor, page: ui.page, selection: ui.selection.clone(), viewport: ui.viewport, playhead: ui.playhead, track_range: ui.track_range }
    }

    /// Whether this peer shows anything in the sequence `item` at all (the sequence tabs always
    /// show everyone; the Timeline only the peers on the same sequence).
    #[must_use]
    pub fn on_sequence(&self, item: u64) -> bool {
        self.page == Some(item)
    }

    /// The peer's playhead as a frame number in a sequence whose settings say `fps` (for the
    /// timeline ruler's peer markers). Ticks are canonical; the frame is a projection.
    #[must_use]
    pub fn playhead_frame(&self, ticks_per_frame: u64) -> Option<u64> {
        if ticks_per_frame == 0 {
            return None;
        }
        self.playhead.map(|t| t / ticks_per_frame)
    }
}
