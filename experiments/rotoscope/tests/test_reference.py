from types import SimpleNamespace

import numpy as np
import pytest
from matting import refine, trimap_from_mask, unknown_tiles
from media import Clip, composite, load_clip, read_alpha, write_frame
from models import verify
from PIL import Image
from pipeline import Runtime, choose_device, run, validate_prompts


@pytest.fixture
def clip(tmp_path):
    return Clip(
        tmp_path, tmp_path / "source.mp4", "fixture", 12, 100, 80, 100, 80, "25"
    )


def test_refinement_never_changes_definite_foreground_or_background():
    mask = np.zeros((96, 128), bool)
    mask[20:80, 25:105] = True
    trimap = trimap_from_mask(mask, 5)
    alpha, tiles = refine(
        np.zeros((96, 128, 3), np.uint8),
        trimap,
        lambda rgb, tri: np.full(tri.shape, 0.375, np.float32),
    )
    assert tiles > 0
    assert np.all(alpha[trimap == 255] == 1)
    assert np.all(alpha[trimap == 0] == 0)
    assert np.all(alpha[trimap == 128] == 0.375)


def test_brush_expands_unknown_region_without_changing_other_pixels():
    mask = np.zeros((100, 100), bool)
    mask[20:80, 20:80] = True
    plain = trimap_from_mask(mask, 3)
    brush = np.zeros_like(mask)
    brush[5:10, 5:10] = True
    painted = trimap_from_mask(mask, 3, brush)
    assert np.all(painted[brush] == 128)
    assert np.array_equal(plain[~brush], painted[~brush])


def test_tiles_cover_every_unknown_pixel_once_and_remain_bounded():
    trimap = np.full((817, 1023), 128, np.uint8)
    coverage = np.zeros(trimap.shape, np.uint8)
    for (x, y, x1, y1), (rx, ry, rx1, ry1) in unknown_tiles(trimap):
        assert 0 <= rx <= x < x1 <= rx1 <= trimap.shape[1]
        assert 0 <= ry <= y < y1 <= ry1 <= trimap.shape[0]
        assert rx1 - rx <= 512 and ry1 - ry <= 512
        coverage[y:y1, x:x1] += 1
    assert np.all(coverage == 1)


def test_constant_trimap_skips_model_inference():
    def forbidden(*_args):
        pytest.fail("No model is needed without unknown pixels")

    for value in [0, 255]:
        trimap = np.full((40, 30), value, np.uint8)
        alpha, tiles = refine(np.zeros((40, 30, 3), np.uint8), trimap, forbidden)
        assert tiles == 0
        assert np.all(alpha == (value / 255))


@pytest.mark.parametrize("prediction", [np.full((48, 48), np.nan), np.zeros((1, 1))])
def test_bad_model_output_is_an_actionable_error(prediction):
    with pytest.raises(ValueError, match="invalid alpha"):
        refine(
            np.zeros((48, 48, 3), np.uint8),
            np.full((48, 48), 128, np.uint8),
            lambda *_: prediction,
        )


def test_16_bit_alpha_and_transparent_png_round_trip(tmp_path):
    for folder in ["masks", "alpha", "rgba", "preview", "overlay"]:
        (tmp_path / folder).mkdir()
    rgb = np.full((20, 20, 3), [100, 150, 200], np.uint8)
    alpha = np.linspace(0, 1, 400, dtype=np.float32).reshape(20, 20)
    write_frame(tmp_path, 0, rgb, alpha > 0.5, alpha)
    recovered = read_alpha(tmp_path / "alpha" / "00000.png")
    assert np.max(np.abs(alpha - recovered)) <= 0.5 / 65535 + 1e-7
    with Image.open(tmp_path / "rgba" / "00000.png") as image:
        rgba = np.asarray(image)
    assert np.array_equal(rgba[..., :3], rgb)
    assert np.max(np.abs(rgba[..., 3] / 255 - alpha)) <= 0.5 / 255 + 1e-7


@pytest.mark.parametrize(
    "prompt",
    [
        {"frame": -1, "points": [[30, 40, 1]]},
        {"frame": 12, "points": [[30, 40, 1]]},
        {"frame": 0, "points": [[float("nan"), 40, 1]]},
        {"frame": 0, "points": [[100, 40, 1]]},
        {"frame": 0, "points": [[30, 40, 0]]},
        {"frame": 0, "points": [[30, 40, 0.5]]},
        {"frame": 0, "box": [40, 40, 20, 20]},
        {"frame": 0, "box": [0, 0, 101, 80]},
    ],
)
def test_hostile_prompts_are_rejected(clip, prompt):
    with pytest.raises(ValueError):
        validate_prompts(clip, [prompt])


def test_box_can_prompt_a_mid_video_frame(clip):
    prompt = validate_prompts(clip, [{"frame": 6, "box": [5, 5, 80, 70]}])
    assert prompt[0]["frame"] == 6
    assert prompt[0]["points"] == []


def test_cancellation_discards_incomplete_cache(clip, monkeypatch, tmp_path):
    import pipeline

    monkeypatch.setattr(pipeline, "DATA", tmp_path)
    runtime = SimpleNamespace(device="cpu", predictor=None, prepare=lambda **_: None)
    with pytest.raises(ValueError, match="Cancelled"):
        run(
            clip,
            [{"frame": 0, "points": [[30, 40, 1]]}],
            runtime,
            cancelled=lambda: True,
        )
    assert not list((tmp_path / "results").glob("*/benchmark.json"))
    assert not list((tmp_path / "results").glob("job-*"))


def test_unreadable_media_and_missing_weight_are_errors(tmp_path):
    corrupt = tmp_path / "corrupt.mp4"
    corrupt.write_bytes(b"not a video")
    with pytest.raises(ValueError, match="could not process"):
        load_clip(corrupt)
    with pytest.raises(ValueError, match="Missing"):
        verify(tmp_path / "missing.pt", "unavailable")


def test_compositing_checks_shape_and_alpha():
    rgb = np.full((5, 6, 3), 200, np.uint8)
    assert np.array_equal(composite(rgb, np.ones((5, 6))), rgb)
    assert np.array_equal(
        composite(rgb, np.zeros((5, 6)), np.zeros_like(rgb)), np.zeros_like(rgb)
    )
    with pytest.raises(ValueError):
        composite(rgb, np.ones((6, 5)))
    with pytest.raises(ValueError):
        composite(rgb, np.full((5, 6), np.inf))


def test_cpu_is_explicit_and_unknown_backend_is_rejected():
    assert choose_device("cpu") == "cpu"
    with pytest.raises(ValueError):
        choose_device("imaginary")


def test_refinement_device_is_independent_and_cpu_remains_explicit(monkeypatch):
    import torch

    monkeypatch.setattr(torch.backends.mps, "is_available", lambda: True)
    assert Runtime("cpu").matte_device == "cpu"
    hybrid = Runtime("cpu", refinement="auto")
    assert hybrid.device == "cpu" and hybrid.matte_device == "mps"
    monkeypatch.setattr(torch.backends.mps, "is_available", lambda: False)
    assert Runtime("cpu", refinement="auto").matte_device == "cpu"
    with pytest.raises(ValueError, match="Metal is unavailable"):
        Runtime("cpu", refinement="mps")


def test_automatic_gpu_tile_failure_falls_back_to_cpu(monkeypatch):
    import torch

    monkeypatch.setattr(torch.backends.mps, "is_available", lambda: True)
    runtime = Runtime("cpu", refinement="auto")

    class Inputs(dict):
        def to(self, device):
            self["device"] = device
            return self

    class Matte:
        def cpu(self):
            return self

        def __call__(self, device):
            if device == "mps":
                raise RuntimeError("Synthetic unsupported GPU operation")
            return SimpleNamespace(alphas=torch.full((1, 1, 32, 32), 0.5))

    runtime.matte = Matte()
    runtime.processor = lambda **_params: Inputs()
    prediction = runtime.infer_tile(
        np.zeros((32, 32, 3), np.uint8), np.full((32, 32), 128, np.uint8)
    )
    assert runtime.matte_device == "cpu" and np.all(prediction == 0.5)
    runtime.matte_device = "mps"
    runtime.automatic_refinement = False
    with pytest.raises(RuntimeError, match="Synthetic unsupported"):
        runtime.infer_tile(
            np.zeros((32, 32, 3), np.uint8), np.full((32, 32), 128, np.uint8)
        )
