//! Load-time migration for projects saved before stills applied EXIF Orientation (#4613,
//! the limitation documented as G10 in docs/gaps.md).
//!
//! A build from before #1583 stored a still's size as the file stores it, unturned, and a
//! clip placed from the still baked its Motion anchor point from that size. The picture
//! itself has decoded upright ever since #1583, so such a project reopens with the still
//! scaled up by its aspect ratio and off-centre. The fix a fresh import gets for free, a
//! saved project must be given on load: swap the stored size to the one the file reports
//! today, and turn the anchor points of clips placed from the still to match.
//!
//! The legacy state is recognised by its shape, not by the file's schema version: a
//! project re-saved by a newer build keeps its stale sizes at the current version, so a
//! version gate would miss exactly the projects that were opened and re-saved since. A
//! still is legacy when its stored size is the transpose of the size its own file decodes
//! to now. A fresh import stores the decoded size, so it can never match; a square still
//! is its own transpose and is skipped; anything whose file cannot be read keeps its
//! stored size until it is relinked and the project is opened again.

use std::collections::HashSet;

use filmcraft_media::MediaKind;
use filmcraft_project::{ItemId, ItemKind, Param, ParamValue, Project, TrackKind};
use image::metadata::Orientation;

use crate::{MediaPool, Services};

/// Swap the stored size of every legacy still in `p` and transpose the Motion anchor of
/// each clip placed from one. Runs while a project is installed, before it is presented,
/// so no undo step is created. The file behind a still is only probed for the size and
/// orientation its decoder reports ([`MediaPool::still_geometry`]): installing a project
/// never decodes a picture for this, and the pool retains no source for the still.
pub fn migrate_legacy_still_sizes(p: &mut Project, pool: &MediaPool, services: &dyn Services) {
    let legacy: Vec<(ItemId, Orientation)> = p
        .items
        .iter()
        .filter_map(|(id, item)| {
            let m = item.as_media()?;
            if m.info.kind != MediaKind::Still {
                return None;
            }
            let v = m.info.video.as_ref()?;
            if v.width == v.height {
                return None;
            }
            let stored = (v.width, v.height);
            let (w, h, orientation) = pool.still_geometry(p, *id, services)?;
            let transposing = matches!(orientation, Orientation::Rotate90 | Orientation::Rotate90FlipH | Orientation::Rotate270 | Orientation::Rotate270FlipH);
            (transposing && (w, h) == (stored.1, stored.0)).then_some((*id, orientation))
        })
        .collect();
    for (id, orientation) in legacy {
        let stored = p.item(id).and_then(|i| i.as_media()).and_then(|m| m.info.video.as_ref()).map(|v| (v.width, v.height));
        if let Some(v) = p.item_mut(id).and_then(|i| i.as_media_mut()).and_then(|m| m.info.video.as_mut()) {
            std::mem::swap(&mut v.width, &mut v.height);
        }
        if let Some(stored) = stored {
            transpose_anchors(p, id, stored, orientation);
        }
    }
}

/// Turn one anchor point from the file's stored pixel coordinates to the upright
/// picture's, for the EXIF Orientation the decoder applied: the same mapping the
/// decoder applies to the pixels (crates/media/tests/still_orientation.rs), in
/// continuous coordinates. A centre point is invariant under all four mappings,
/// which is why the orientation only matters for a noncentre anchor. Anything
/// that is not a transposing orientation falls back to the plain swap the
/// migration used before the orientation was known: a legacy still with no
/// transposing orientation means the file at its path changed since the project
/// was saved, and the swap is the best mapping available for it.
fn turn_point(x: f64, y: f64, stored: (u32, u32), orientation: Orientation) -> (f64, f64) {
    let (w, h) = (f64::from(stored.0), f64::from(stored.1));
    match orientation {
        Orientation::Rotate90FlipH => (y, x),
        Orientation::Rotate90 => (h - y, x),
        Orientation::Rotate270FlipH => (h - y, w - x),
        Orientation::Rotate270 => (y, w - x),
        _ => (y, x),
    }
}

/// The anchor point is measured in source pixels, so turning the source turns it too,
/// for the static value and for every keyframe. A coordinate left on "auto" (NaN)
/// stays auto and resolves against the new size. Position is measured in sequence
/// pixels and does not move. Clips placed from a subclip of the still keep the
/// subclip's id, so they are transposed too: anything that resolves to the still
/// through [`Project::resolve_media`] is one of its clips.
///
/// One saved value is not a point in the still's coordinates at all. A build from
/// before the subclip source-size fix had no size for a subclip, so placing one
/// resolved the anchor against a 1920 x 1080 fallback frame and saved its centre,
/// [`LEGACY_SUBCLIP_DEFAULT_ANCHOR`]: for a 40 x 20 still that point sits far outside
/// the picture, and turning it like a stored point throws it further out still. On a
/// clip placed from a subclip, an anchor still holding exactly that value with no
/// keyframes is that saved default, so it migrates to the upright centre instead.
/// An anchor the user edited or keyframed is a control they set deliberately and
/// turns like any other point; a direct placement never holds the fallback value as
/// a default, because it baked the still's own centre, which turns to the upright
/// centre on its own.
fn transpose_anchors(p: &mut Project, item: ItemId, stored: (u32, u32), orientation: Orientation) {
    let targets: HashSet<ItemId> = p.items.keys().filter(|id| p.resolve_media(**id).is_some_and(|(m, _, _)| m == item)).copied().collect();
    let centre = (f64::from(stored.1) / 2.0, f64::from(stored.0) / 2.0);
    for it in p.items.values_mut() {
        let ItemKind::Sequence(q) = &mut it.kind else { continue };
        for t in std::sync::Arc::make_mut(q).tracks_mut(TrackKind::Video) {
            for ti in &mut t.items {
                if !targets.contains(&ti.item) {
                    continue;
                }
                for e in &mut ti.effects {
                    if e.effect == "motion"
                        && let Some(param) = e.param_mut("anchor")
                    {
                        transpose_point(param, stored, orientation, (ti.item != item).then_some(centre));
                    }
                }
            }
        }
    }
}

/// The centre of the 1920 x 1080 fallback frame a build from before the subclip
/// source-size fix resolved a subclip placement's anchor against (see
/// [`transpose_anchors`]).
const LEGACY_SUBCLIP_DEFAULT_ANCHOR: (f64, f64) = (960.0, 540.0);

fn transpose_point(param: &mut Param, stored: (u32, u32), orientation: Orientation, subclip_centre: Option<(f64, f64)>) {
    if let Some(centre) = subclip_centre
        && param.keyframes.is_empty()
        && let ParamValue::Vec2(v) = &mut param.value
        && (v.x, v.y) == LEGACY_SUBCLIP_DEFAULT_ANCHOR
    {
        v.x = centre.0;
        v.y = centre.1;
        return;
    }
    fn transpose(value: &mut ParamValue, stored: (u32, u32), orientation: Orientation) {
        if let ParamValue::Vec2(v) = value
            && !v.x.is_nan()
            && !v.y.is_nan()
        {
            let (x, y) = turn_point(v.x, v.y, stored, orientation);
            v.x = x;
            v.y = y;
        }
    }
    transpose(&mut param.value, stored, orientation);
    for k in &mut param.keyframes {
        transpose(&mut k.value, stored, orientation);
    }
}
