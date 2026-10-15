"""Trimap construction and bounded padded-ROI alpha refinement."""

import numpy as np
from scipy.ndimage import maximum_filter, minimum_filter


class Cancelled(ValueError):
    """An intentional stop, rather than an inference failure."""


def trimap_from_mask(mask, radius=8, brush=None):
    mask = np.asarray(mask, dtype=bool)
    if mask.ndim != 2 or min(mask.shape) == 0:
        raise ValueError("Mask must be a nonempty two-dimensional image.")
    if not isinstance(radius, int) or not 1 <= radius <= 32:
        raise ValueError("Edge band must be an integer from 1 to 32 pixels.")
    size = 2 * radius + 1
    inside = minimum_filter(mask, size=size, mode="constant", cval=0)
    outside = ~maximum_filter(mask, size=size, mode="constant", cval=0)
    trimap = np.full(mask.shape, 128, dtype=np.uint8)
    trimap[outside] = 0
    trimap[inside] = 255
    if brush is not None:
        brush = np.asarray(brush, dtype=bool)
        if brush.shape != mask.shape:
            raise ValueError("Refine brush must match the loaded frame.")
        trimap[brush] = 128
    return trimap


def unknown_tiles(trimap, core=384, padding=64):
    if core < 32 or not 0 <= padding <= 128:
        raise ValueError("Invalid tile dimensions.")
    height, width = trimap.shape
    for y in range(0, height, core):
        for x in range(0, width, core):
            y1, x1 = min(y + core, height), min(x + core, width)
            if not np.any(trimap[y:y1, x:x1] == 128):
                continue
            yield (
                (x, y, x1, y1),
                (
                    max(0, x - padding),
                    max(0, y - padding),
                    min(width, x1 + padding),
                    min(height, y1 + padding),
                ),
            )


def refine(rgb, trimap, infer_tile, cancelled=lambda: False):
    if rgb.shape[:2] != trimap.shape:
        raise ValueError("Trimap and RGB dimensions differ.")
    alpha = (trimap == 255).astype(np.float32)
    tiles = 0
    for core, roi in unknown_tiles(trimap):
        if cancelled():
            raise Cancelled("Cancelled; no completed result was cached.")
        x0, y0, x1, y1 = roi
        prediction = np.asarray(
            infer_tile(rgb[y0:y1, x0:x1], trimap[y0:y1, x0:x1]), dtype=np.float32
        )
        if prediction.shape != (y1 - y0, x1 - x0) or not np.isfinite(prediction).all():
            raise ValueError("ViTMatte returned invalid alpha.")
        cx0, cy0, cx1, cy1 = core
        values = prediction[cy0 - y0 : cy1 - y0, cx0 - x0 : cx1 - x0]
        target = alpha[cy0:cy1, cx0:cx1]
        uncertain = trimap[cy0:cy1, cx0:cx1] == 128
        target[uncertain] = np.clip(values[uncertain], 0, 1)
        tiles += 1
    return alpha, tiles
