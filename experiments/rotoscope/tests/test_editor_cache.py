import copy
import threading
from pathlib import Path
from types import SimpleNamespace

import editor_worker as worker
import numpy as np
import pytest
import torch
from inference_cache import BudgetLRU, state_bytes
from PIL import Image


class Predictor:
    def __init__(self):
        self.starts = 0
        self.tracks = 0
        self.resets = 0
        self.cancel = None

    def init_state(self, directory, **_options):
        self.starts += 1
        with Image.open(f"{directory}/00000.jpg") as image:
            width, height = image.size
        return {
            "width": width,
            "height": height,
            "points": {},
            "frames": len(list(Path(directory).glob("*.jpg"))),
            "images": torch.zeros((1, 3, 8, 8)),
            "cached_features": {0: torch.zeros((1, 4, 4, 4))},
        }

    def add_new_points_or_box(self, state, frame_idx, obj_id, points, labels):
        assert obj_id == 1
        state["points"][frame_idx] = list(zip(points, labels, strict=True))

    def propagate_in_video(self, state, **_options):
        self.tracks += 1
        mask = np.zeros((state["height"], state["width"]), bool)
        yy, xx = np.ogrid[: state["height"], : state["width"]]
        for (x, y), label in state["points"][0]:
            inside = (xx - x) ** 2 + (yy - y) ** 2 < 8**2
            mask[inside] = bool(label)
        if self.cancel is not None:
            self.cancel.set()
        for i in range(state["frames"]):
            yield i, [1], torch.from_numpy(np.where(mask, 1.0, -1.0))[None, None]

    def reset_state(self, state):
        assert worker.INFERENCE.locked(), "state cleanup must finish before reuse"
        self.resets += 1
        state["points"].clear()


@pytest.fixture
def setup(tmp_path, monkeypatch):
    predictor = Predictor()
    runtime = SimpleNamespace(
        device="cpu",
        matte_device="cpu",
        predictor=predictor,
        prepare=lambda **_options: None,
        infer_tile=lambda _rgb, trimap: np.full(trimap.shape, 0.5, np.float32),
    )
    monkeypatch.setattr(worker, "RUNTIME", runtime)
    monkeypatch.setattr(worker, "DATA", tmp_path / "cache")
    monkeypatch.setattr(worker, "FEATURES", BudgetLRU(2, 4096))
    monkeypatch.setattr(worker, "TRACKED", BudgetLRU(4, 40_000))
    monkeypatch.setattr(worker, "ALPHAS", BudgetLRU(120, 80_000))
    directory = tmp_path / "frames"
    directory.mkdir()
    Image.new("RGB", (80, 60), (10, 20, 30)).save(directory / "00000.ppm")
    data = {
        "version": 1,
        "directory": str(directory),
        "width": 80,
        "height": 60,
        "times": [0],
        "end": 1,
        "fps": "25/1",
        "points": [{"time": 0, "x": 20.0, "y": 20.0, "kind": "foreground"}],
        "quality": False,
        "band": 2,
    }
    return predictor, directory, data


def full_clip(directory, data):
    for i in range(3):
        Image.new("RGB", (80, 60), (10 + i * 40, 20, 30)).save(
            directory / f"{i:05d}.ppm"
        )
    data.update(quality=True, times=[0, 1, 2], end=3)
    return data


def infer(directory, data, cancelled=None):
    job = {"cancel": cancelled or threading.Event(), "done": 0, "status": "Queued"}
    worker.infer(job, data, directory)
    return job


def test_lru_eviction_respects_both_budgets_and_replacement():
    cache = BudgetLRU(2, 10)
    cache.put("a", "first", 4)
    cache.put("b", "second", 4)
    assert cache.get("a") == "first"
    cache.put("c", "third", 4)
    assert cache.get("b") is None
    cache.put("a", "replacement", 9)
    assert cache.get("c") is None and cache.bytes == 9
    cache.put("too_large", "unused", 11)
    assert cache.bytes == 9 and cache.get("too_large") is None
    cache.discard("a")
    assert cache.bytes == 0


def test_feature_storage_counts_aliases_only_once():
    tensor = torch.zeros((4, 4))
    assert (
        state_bytes({"images": tensor, "cached_features": {0: tensor.view(2, 8)}}) == 64
    )


def test_new_points_reuse_features_without_reusing_old_prompts(setup):
    predictor, directory, data = setup
    first = infer(directory, data)
    changed = copy.deepcopy(data)
    changed["points"][0]["x"] = 40.0
    second = infer(directory, changed)
    assert "error" not in first and "error" not in second
    assert predictor.starts == 1 and predictor.tracks == 2 and predictor.resets == 2
    assert second["metrics"]["features_cache_hit"]
    assert not second["metrics"]["tracking_cache_hit"]
    assert first["mask"]["frames"] != second["mask"]["frames"]


def test_quality_band_brush_and_timestamps_reuse_tracking_but_not_alpha(setup):
    predictor, directory, data = setup
    first = infer(directory, data)
    changed = copy.deepcopy(data)
    changed.update(quality=True, band=3, times=[100], end=101)
    changed["points"][0]["time"] = 100
    changed["points"].append({"time": 100, "x": 10.0, "y": 10.0, "kind": "refine"})
    second = infer(directory, changed)
    assert "error" not in first and "error" not in second
    assert second["metrics"]["tracking_cache_hit"]
    assert not second["metrics"]["alpha_cache_hit"]
    assert predictor.tracks == 1
    assert second["mask"]["quality"] and second["mask"]["end"] == 101
    assert second["mask"]["frames"][0]["time"] == 100
    assert any(0 < value < 65535 for _, value in second["mask"]["frames"][0]["runs"])


def test_pixel_changes_invalidate_features_and_tracking(setup):
    predictor, directory, data = setup
    infer(directory, data)
    with Image.open(directory / "00000.ppm") as source:
        image = source.copy()
    image.putpixel((0, 0), (255, 0, 0))
    image.save(directory / "00000.ppm")
    changed = infer(directory, data)
    assert "error" not in changed
    assert not changed["metrics"]["features_cache_hit"]
    assert not changed["metrics"]["tracking_cache_hit"]
    assert predictor.starts == 2 and predictor.tracks == 2


def test_cancelled_tracking_cleans_state_and_does_not_cache_partial_masks(setup):
    predictor, directory, data = setup
    cancelled = threading.Event()
    predictor.cancel = cancelled
    first = infer(directory, data, cancelled)
    assert first["error"] == "Cancelled" and "mask" not in first
    assert not worker.TRACKED.items
    assert not list(worker.DATA.glob("editor-cache/*.json"))
    predictor.cancel = None
    second = infer(directory, data)
    assert "error" not in second and not second["metrics"]["tracking_cache_hit"]
    assert predictor.resets == 2 and predictor.starts == 1


def test_quality_brush_correction_refines_only_the_changed_frame(setup):
    _, directory, data = setup
    full_clip(directory, data)
    first = infer(directory, data)
    changed = copy.deepcopy(data)
    changed["points"].append({"time": 1, "x": 55.0, "y": 40.0, "kind": "refine"})
    second = infer(directory, changed)
    assert "error" not in second
    assert second["metrics"]["tracking_cache_hit"]
    assert second["metrics"]["alpha_frames_reused"] == 2
    assert second["mask"]["frames"][0] == first["mask"]["frames"][0]
    assert second["mask"]["frames"][2] == first["mask"]["frames"][2]
    assert second["mask"]["frames"][1] != first["mask"]["frames"][1]
    assert all(not value.flags.writeable for value, _ in worker.ALPHAS.items.values())


def test_frame_alpha_cache_restamps_time_without_repeating_refinement(setup):
    _, directory, data = setup
    first = infer(directory, full_clip(directory, data))
    changed = copy.deepcopy(data)
    changed.update(times=[100, 101, 102], end=103)
    changed["points"][0]["time"] = 100
    second = infer(directory, changed)
    assert second["metrics"]["alpha_frames_reused"] == 3
    assert [f["time"] for f in second["mask"]["frames"]] == [100, 101, 102]
    assert [f["runs"] for f in first["mask"]["frames"]] == [
        f["runs"] for f in second["mask"]["frames"]
    ]


def test_band_source_and_backend_changes_invalidate_the_relevant_alpha(setup):
    _, directory, data = setup
    infer(directory, full_clip(directory, data))
    changed = copy.deepcopy(data)
    changed["band"] = 3
    assert infer(directory, changed)["metrics"]["alpha_frames_reused"] == 0
    Image.new("RGB", (80, 60), (255, 255, 255)).save(directory / "00001.ppm")
    source_changed = infer(directory, changed)
    assert source_changed["metrics"]["alpha_frames_reused"] == 2
    worker.RUNTIME.matte_device = "mps"
    device_changed = infer(directory, changed)
    assert device_changed["metrics"]["alpha_frames_reused"] == 0
    assert device_changed["metrics"]["tracking_cache_hit"]


def test_cancelled_refinement_publishes_no_new_frame_alpha(setup):
    _, directory, data = setup
    full_clip(directory, data)
    cancel = threading.Event()

    def tile(_rgb, trimap):
        cancel.set()
        return np.full(trimap.shape, 0.5, np.float32)

    worker.RUNTIME.infer_tile = tile
    result = infer(directory, data, cancel)
    assert result["error"] == "Cancelled" and "mask" not in result
    assert not worker.ALPHAS.items
    assert not list(worker.DATA.glob("editor-cache/*.json"))


def test_gpu_failure_restarts_refinement_and_caches_only_cpu_alpha(setup):
    _, directory, data = setup
    full_clip(directory, data)
    worker.RUNTIME.matte_device = "mps"
    calls = []

    def tile(_rgb, trimap):
        calls.append(worker.RUNTIME.matte_device)
        if len(calls) == 2:
            worker.RUNTIME.matte_device = "cpu"
        value = 0.5 if worker.RUNTIME.matte_device == "cpu" else 0.25
        return np.full(trimap.shape, value, np.float32)

    worker.RUNTIME.infer_tile = tile
    result = infer(directory, data)
    assert "error" not in result
    assert calls == ["mps", "mps", "cpu", "cpu", "cpu"]
    assert result["metrics"]["refinement_device"] == "cpu"
    assert all(
        value in {0, 32768, 65535}
        for frame in result["mask"]["frames"]
        for _, value in frame["runs"]
    )
    assert len(worker.ALPHAS.items) == 3
    assert infer(directory, data)["metrics"]["alpha_cache_hit"]
