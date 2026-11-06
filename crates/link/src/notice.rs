//! The notice after `link.editIn`: the nest and the hand-off already happened, and the user can keep
//! or undo them.
//!
//! `link.editIn` never asks first. It nests, opens EffectCraft, and reports what EffectCraft cannot
//! show. [`LinkNotice::from_reply`] turns that report into a banner the UI draws without blocking.
//! Continue drops the banner. Undo calls [`undo`]: one undo step puts back the clip, its effects
//! and the selection, and EffectCraft unbinds the comp. EffectCraft keeps the comp and every edit
//! made in it, so Undo never throws EffectCraft work away.

use std::sync::{Mutex, PoisonError};

use filmcraft_engine::Session;
use serde_json::Value;

use crate::edit_in::LinkPeer;
use crate::lift::{dropped_name, is_effect_drop};
use crate::prompt::Unsupported;

/// The sentence on the banner. `FilmCraft` and `EffectCraft` are drawn as branded names.
pub const CONTINUE_TEXT: &str = "Click Continue to nest your clip. This keeps the effect in FilmCraft, but you will not see it applied in EffectCraft.";
/// Label of the line that lists the effects EffectCraft cannot import.
pub const EFFECTS_LABEL: &str = "Unimportable FilmCraft effects:";
/// Label of the keep button.
pub const CONTINUE_LABEL: &str = "Continue";
/// Label of the undo button.
pub const UNDO_LABEL: &str = "Undo";

/// Undo step label of `link.editIn`.
pub(crate) const STEP: &str = "link.editIn";

/// What the user is asked to keep or undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkNotice {
    pub link_id: String,
    /// EffectCraft instance that holds the link.
    pub instance: String,
    /// Display names of effects that moved to the outer clip.
    pub lifted: Vec<String>,
    /// Parts EffectCraft reported as not kept.
    pub dropped: Vec<Unsupported>,
}

impl LinkNotice {
    /// The notice for a `link.editIn` reply. `None` when nothing was lifted and nothing was dropped.
    pub fn from_reply(reply: &Value) -> Option<Self> {
        let lifted: Vec<String> = reply.get("lifted_effects").and_then(Value::as_array)?.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        let dropped: Vec<Unsupported> = reply.get("unsupported").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        if lifted.is_empty() && dropped.is_empty() {
            return None;
        }
        Some(Self {
            link_id: reply.get("link_id")?.as_str()?.to_string(),
            instance: reply.get("instance").and_then(Value::as_str).unwrap_or_default().to_string(),
            lifted,
            dropped,
        })
    }

    /// Display names of every effect EffectCraft cannot import: the ones that moved and the ones that did not.
    pub fn effects(&self) -> Vec<String> {
        let mut names = self.lifted.clone();
        for name in self.unlifted() {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    /// Effects that stayed inside the nest. After the swap the timeline plays the EffectCraft result,
    /// so these do not show there until the user undoes the nest.
    pub fn unlifted(&self) -> Vec<String> {
        self.dropped.iter().filter(|part| is_effect_drop(part)).map(dropped_name).collect()
    }

    /// One line per fact, in plain words. The banner shows them under [`CONTINUE_TEXT`]. The UI draws
    /// the same lines from the translated templates.
    pub fn details(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let effects = self.effects();
        if !effects.is_empty() {
            lines.push(format!("{EFFECTS_LABEL} {}", effects.join(", ")));
        }
        lines.extend(self.dropped.iter().filter(|part| !is_effect_drop(part)).map(Unsupported::line));
        lines
    }
}

static SLOT: Mutex<Option<LinkNotice>> = Mutex::new(None);

/// Leave `notice` for the UI. A later notice replaces an earlier one, which counts as kept.
pub fn set(notice: LinkNotice) {
    *SLOT.lock().unwrap_or_else(PoisonError::into_inner) = Some(notice);
}

/// The waiting notice, without removing it.
pub fn peek() -> Option<LinkNotice> {
    SLOT.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Remove and return the waiting notice.
pub fn take() -> Option<LinkNotice> {
    SLOT.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// True while the newest undo step is the `link.editIn` step, so Undo reverts nothing else.
pub fn undoable(session: &Session) -> bool {
    session.history.undo.last().is_some_and(|(label, _, _)| label == STEP)
}

/// Undo the nest and tell EffectCraft to unbind. `peer` `None` means EffectCraft cannot be reached.
///
/// The project is restored first, in one undo step. A peer failure never blocks the restore, and
/// the text says where the comp stays. `Err` only when the newest step is not the link step, and
/// then nothing changes.
pub fn undo(session: &mut Session, peer: Option<&mut dyn LinkPeer>, notice: &LinkNotice) -> Result<String, String> {
    if !undoable(session) {
        return Err("Cannot undo the nest: other edits came after it. Use Edit > Undo or Unlink from EffectCraft.".to_string());
    }
    session.undo();
    let tail = match peer {
        None => " EffectCraft could not be reached, so its comp stays linked there.".to_string(),
        Some(peer) => match peer.close(&notice.link_id) {
            Ok(closed) if closed.edited => {
                let name = if closed.comp.is_empty() { "the comp".to_string() } else { format!("\"{}\"", closed.comp) };
                format!(" EffectCraft keeps {name} with your edits.")
            }
            Ok(_) => String::new(),
            Err(err) => format!(" EffectCraft did not unlink: {err}. Its comp stays there."),
        },
    };
    Ok(format!("Undone. Your clip is back as it was.{tail}"))
}
