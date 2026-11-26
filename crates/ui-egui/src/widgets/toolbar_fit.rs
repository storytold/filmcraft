//! Fitting a row of toolbar groups into the room it has, with an overflow menu for the rest.

/// Visible toolbar groups and the groups to present in the caller's overflow menu.
///
/// Groups stay at their natural widths, in their original order. The caller must make every
/// index in `overflow` available through an enabled, focusable overflow control and its menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolbarFit {
    /// A leading sequence of `(input index, natural width)` pairs. Groups are never compressed.
    pub shown: Vec<(usize, f32)>,
    /// Remaining input indices, in order. Empty when the whole toolbar fits.
    pub overflow: Vec<usize>,
    /// Full requested overflow-control width when needed, otherwise zero.
    pub overflow_width: f32,
}

const MAX_GROUPS: usize = 4096;
const MAX_EXTENT: f32 = 1_000_000.0;

fn nonnegative_extent(value: f32) -> bool {
    value.is_finite() && (0.0..=MAX_EXTENT).contains(&value)
}

/// Fit indivisible toolbar groups and, when needed, a full-size overflow control.
///
/// `spacing` is included between adjacent visible groups and before the overflow control. The
/// result keeps a leading sequence instead of skipping a wide group to show later commands. At
/// most 4096 groups and dimensions of at most one million points are accepted; all dimensions
/// must be finite, widths strictly positive, and `available` and `spacing` nonnegative.
///
/// Returns `None` for invalid input, or when overflow is needed but `available` is smaller than
/// `overflow_width`. The caller then uses an explicit alternative (a scrollable row) and never
/// hides the commands silently.
pub fn fit_toolbar(natural: &[f32], available: f32, spacing: f32, overflow_width: f32) -> Option<ToolbarFit> {
    if natural.len() > MAX_GROUPS
        || !nonnegative_extent(available)
        || !nonnegative_extent(spacing)
        || !nonnegative_extent(overflow_width)
        || overflow_width == 0.0
        || natural.iter().any(|&width| !nonnegative_extent(width) || width == 0.0)
    {
        return None;
    }
    let gaps = natural.len().saturating_sub(1);
    let total = natural.iter().map(|&width| f64::from(width)).sum::<f64>() + f64::from(spacing) * gaps as f64;
    if total <= f64::from(available) {
        return Some(ToolbarFit { shown: natural.iter().copied().enumerate().collect(), ..Default::default() });
    }
    if available < overflow_width {
        return None;
    }
    let budget = f64::from(available) - f64::from(overflow_width);
    let mut used = 0.0;
    let shown_count = natural
        .iter()
        .take_while(|&&width| {
            // Every shown group has a following gap: to the next group or to the overflow.
            let next = used + f64::from(width) + f64::from(spacing);
            if next <= budget {
                used = next;
                true
            } else {
                false
            }
        })
        .count();
    Some(ToolbarFit {
        shown: natural.iter().copied().take(shown_count).enumerate().collect(),
        overflow: (shown_count..natural.len()).collect(),
        overflow_width,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the budget arithmetic: a shown group's gap before the overflow control, and the
    /// order of groups between the row and the menu.
    #[test]
    fn every_group_remains_reachable_and_every_accepted_fit_stays_within_budget() {
        let widths = [12.5, 28.0, 24.25, 58.0, 32.0];
        for available in [20.0, 31.5, 37.0, 55.0, 93.5, 156.0, 500.0, MAX_EXTENT] {
            for spacing in [0.0, 0.5, 4.0, 12.0] {
                let fit = fit_toolbar(&widths, available, spacing, 20.0).unwrap();
                let indices: Vec<_> = fit.shown.iter().map(|&(i, _)| i).chain(fit.overflow.iter().copied()).collect();
                assert_eq!(indices, [0, 1, 2, 3, 4]);
                for &(index, width) in &fit.shown {
                    assert_eq!(width.to_bits(), widths[index].to_bits());
                }
                let count = fit.shown.len() + usize::from(!fit.overflow.is_empty());
                let used = fit.shown.iter().map(|&(_, width)| f64::from(width)).sum::<f64>()
                    + f64::from(fit.overflow_width)
                    + count.saturating_sub(1) as f64 * f64::from(spacing);
                assert!(used <= f64::from(available), "{fit:?} uses {used} of {available}");
            }
        }
    }

    /// Guards the scrolling fallback: a row too narrow for the overflow control gets no fit.
    #[test]
    fn a_row_narrower_than_the_overflow_control_gets_no_fit() {
        assert!(fit_toolbar(&[30.0, 30.0], 19.0, 4.0, 20.0).is_none());
        assert!(fit_toolbar(&[30.0, 30.0], 20.0, 4.0, 20.0).is_some_and(|fit| fit.shown.is_empty() && fit.overflow.len() == 2));
    }
}
