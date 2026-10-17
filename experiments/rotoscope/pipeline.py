"""Actual EdgeTAM + ViTMatte reference inference, with atomic result caching."""

import json
import resource
import shutil
import sys
import tempfile
import time
from contextlib import nullcontext, redirect_stdout
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile

import numpy as np
import torch
from matting import Cancelled, refine, trimap_from_mask
from media import annotate_outputs, finite_number, key_for, previews, write_frame
from models import DATA, EDGE_REV, EDGE_SHA, MATTE_REV, MATTE_SHA, model_paths
from PIL import Image

PIPELINE_VERSION = 3


def choose_device(requested="auto"):
    if requested not in {"auto", "cpu", "mps", "cuda"}:
        raise ValueError("Device must be auto, cpu, mps or cuda.")
    if requested == "auto":
        # Repeated EdgeTAM runs on the pinned MPS stack returned corrupt logits;
        # CPU is the validated default on Apple Silicon. Explicit MPS remains
        # available for runtime diagnostics, rather than normal editor testing.
        requested = "cuda" if torch.cuda.is_available() else "cpu"
    if requested == "mps" and not torch.backends.mps.is_available():
        raise ValueError("Metal is unavailable. Select CPU.")
    if requested == "cuda" and not torch.cuda.is_available():
        raise ValueError("CUDA is unavailable. Select CPU.")
    return requested


def sync(device):
    if device == "mps":
        torch.mps.synchronize()
    elif device == "cuda":
        torch.cuda.synchronize()


def memory(device):
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    result = {
        "process_lifetime_peak_rss_bytes": rss
        if sys.platform == "darwin"
        else rss * 1024
    }
    if device == "mps":
        result.update(
            mps_current_bytes=torch.mps.current_allocated_memory(),
            mps_driver_bytes=torch.mps.driver_allocated_memory(),
        )
    elif device == "cuda":
        result.update(
            cuda_current_bytes=torch.cuda.memory_allocated(),
            cuda_peak_bytes=torch.cuda.max_memory_allocated(),
        )
    return result


def validate_prompts(clip, prompts):
    if not isinstance(prompts, list) or not 1 <= len(prompts) <= 120:
        raise ValueError(
            "Add foreground points or a bounding box before tracking (maximum 120 prompted frames)."
        )
    seen = set()
    any_foreground = False
    normalized = []
    for prompt in prompts:
        frame = prompt.get("frame")
        if not isinstance(frame, int) or not 0 <= frame < clip.count or frame in seen:
            raise ValueError("Prompt frame must be unique and within the loaded clip.")
        seen.add(frame)
        points = prompt.get("points", [])
        if not isinstance(points, list) or len(points) > 64:
            raise ValueError("Use at most 64 points per frame.")
        clean = []
        for point in points:
            if len(point) != 3 or point[2] not in (0, 1):
                raise ValueError(
                    "Points need x, y and a 0/background or 1/foreground label."
                )
            x, y = finite_number(point[0], "x"), finite_number(point[1], "y")
            if not 0 <= x < clip.width or not 0 <= y < clip.height:
                raise ValueError("Point is outside the loaded frame.")
            clean.append([x, y, int(point[2])])
            any_foreground |= bool(point[2])
        box = prompt.get("box")
        if box is not None:
            if len(box) != 4:
                raise ValueError("A bounding box needs x0,y0,x1,y1.")
            box = [finite_number(v, "box coordinate") for v in box]
            if not (
                0 <= box[0] < box[2] <= clip.width
                and 0 <= box[1] < box[3] <= clip.height
            ):
                raise ValueError(
                    "Bounding box must have positive area and lie within the loaded frame."
                )
            any_foreground = True
        if not clean and box is None:
            raise ValueError("Each prompted frame needs a point or box.")
        normalized.append({"frame": frame, "points": clean, "box": box})
    if not any_foreground:
        raise ValueError("Add at least one foreground point or bounding box.")
    return sorted(normalized, key=lambda value: value["frame"])


class Runtime:
    def __init__(self, requested="auto", refinement="same"):
        self.device = choose_device(requested)
        self.automatic_refinement = refinement == "auto"
        if refinement == "auto":
            refinement = (
                "mps"
                if self.device == "cpu" and torch.backends.mps.is_available()
                else self.device
            )
        self.matte_device = (
            self.device if refinement == "same" else choose_device(refinement)
        )
        if self.device == "cpu":
            torch.set_num_threads(min(4, torch.get_num_threads()))
        self.predictor = None
        self.matte = None
        self.processor = None
        self.model_setup_ms = 0.0

    def prepare(self, quality=False):
        if self.predictor is not None and (not quality or self.matte is not None):
            self.model_setup_ms = 0.0
            return
        start = time.perf_counter()
        edge, matte = model_paths(quality=quality)
        if self.predictor is None:
            from sam2.build_sam import build_sam2_video_predictor

            # The CUDA-only connected-component cleanup is optional. Disable it explicitly
            # instead of invoking an unavailable extension on CPU/Metal for every frame.
            with redirect_stdout(sys.stderr):
                self.predictor = build_sam2_video_predictor(
                    "edgetam.yaml",
                    str(edge),
                    device=self.device,
                    apply_postprocessing=False,
                    hydra_overrides_extra=[
                        "++model.fill_hole_area=0",
                        "++model.sam_mask_decoder_extra_args.dynamic_multimask_via_stability=true",
                    ],
                )
        if quality and self.matte is None:
            from transformers import VitMatteForImageMatting, VitMatteImageProcessor

            self.processor = VitMatteImageProcessor.from_pretrained(
                str(matte), local_files_only=True
            )
            self.matte = VitMatteForImageMatting.from_pretrained(
                str(matte),
                local_files_only=True,
                use_safetensors=True,
            ).eval()
            try:
                self.matte.to(self.matte_device)
            except RuntimeError as error:
                if not self.fallback_refinement(error):
                    raise
        sync(self.device)
        self.model_setup_ms = (time.perf_counter() - start) * 1000

    def fallback_refinement(self, error):
        if not (
            self.automatic_refinement
            and self.device == "cpu"
            and self.matte_device == "mps"
        ):
            return False
        self.matte.cpu()
        self.matte_device = "cpu"
        print(f"GPU refinement unavailable; retrying on CPU: {error}", file=sys.stderr)
        return True

    def infer_tile(self, rgb, trimap):
        inputs = self.processor(
            images=Image.fromarray(rgb),
            trimaps=Image.fromarray(trimap),
            return_tensors="pt",
        )

        def predict():
            alpha = self.matte(**inputs.to(self.matte_device)).alphas[
                0, 0, : rgb.shape[0], : rgb.shape[1]
            ]
            return alpha.float().cpu().numpy()

        try:
            return predict()
        except RuntimeError as error:
            if not self.fallback_refinement(error):
                raise
            return predict()


def run(
    clip,
    prompts,
    runtime,
    mode="fast",
    band=8,
    brushes=None,
    cancelled=lambda: False,
    progress=lambda _value, _message: None,
):
    if mode not in {"fast", "quality"}:
        raise ValueError("Mode must be fast or quality.")
    if not isinstance(band, int) or not 1 <= band <= 32:
        raise ValueError("Edge band must be an integer from 1 to 32 pixels.")
    prompts = validate_prompts(clip, prompts)
    brushes = brushes or {}
    for frame, brush in brushes.items():
        if (
            not isinstance(frame, int)
            or not 0 <= frame < clip.count
            or np.asarray(brush).shape != (clip.height, clip.width)
        ):
            raise ValueError("Refine brush must match a frame in the loaded clip.")
    settings = {
        "pipeline": PIPELINE_VERSION,
        "source_sha256": clip.sha,
        "count": clip.count,
        "dimensions": [clip.width, clip.height],
        "fps": clip.fps,
        "prompts": prompts,
        "mode": mode,
        "band": band,
        "device": runtime.device,
        "edge_source": EDGE_REV,
        "edge_sha256": EDGE_SHA,
        "matte_revision": MATTE_REV if mode == "quality" else None,
        "matte_sha256": MATTE_SHA if mode == "quality" else None,
        "brushes": {
            str(i): key_for(np.packbits(b).tolist()) for i, b in brushes.items()
        },
    }
    key = key_for(settings)
    destination = DATA / "results" / key
    complete = destination / "benchmark.json"
    if complete.exists():
        progress(1.0, "Loaded completed mask cache")
        return destination, {**json.loads(complete.read_text()), "cache_hit": True}
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix="job-", dir=destination.parent))
    state = None
    try:
        for name in ["masks", "alpha", "rgba", "preview", "overlay", "trimaps"]:
            (temporary / name).mkdir()
        progress(0.01, f"Loading models on {runtime.device}")
        runtime.prepare(quality=mode == "quality")
        start = time.perf_counter()
        timings = {}
        samples = [memory(runtime.device)]
        context = (
            torch.autocast("cuda", dtype=torch.bfloat16)
            if runtime.device == "cuda"
            else nullcontext()
        )
        with torch.inference_mode(), context:
            if cancelled():
                raise Cancelled("Cancelled; no completed result was cached.")
            load_start = time.perf_counter()
            state = runtime.predictor.init_state(
                str(clip.directory),
                offload_video_to_cpu=True,
                offload_state_to_cpu=True,
            )
            sync(runtime.device)
            frame_setup_ms = (time.perf_counter() - load_start) * 1000
            prompt_start = time.perf_counter()
            for prompt in prompts:
                if cancelled():
                    raise Cancelled("Cancelled; no completed result was cached.")
                points = prompt["points"]
                runtime.predictor.add_new_points_or_box(
                    state,
                    frame_idx=prompt["frame"],
                    obj_id=1,
                    points=np.asarray([p[:2] for p in points], dtype=np.float32)
                    if points
                    else None,
                    labels=np.asarray([p[2] for p in points], dtype=np.int32)
                    if points
                    else None,
                    box=np.asarray(prompt["box"], dtype=np.float32)
                    if prompt["box"]
                    else None,
                )
            sync(runtime.device)
            prompt_ms = (time.perf_counter() - prompt_start) * 1000
            seed = prompts[0]["frame"]
            for reverse in [False, True] if seed > 0 else [False]:
                generator = iter(
                    runtime.predictor.propagate_in_video(
                        state, start_frame_idx=seed, reverse=reverse
                    )
                )
                while True:
                    if cancelled():
                        raise Cancelled("Cancelled; no completed result was cached.")
                    tick = time.perf_counter()
                    try:
                        index, ids, logits = next(generator)
                    except StopIteration:
                        break
                    mask = (logits[ids.index(1), 0] > 0).cpu().numpy()
                    sync(runtime.device)
                    if index not in timings:
                        timings[index] = {
                            "frame": index,
                            "tracking_ms": (time.perf_counter() - tick) * 1000,
                            "direction": "backward" if reverse else "forward",
                        }
                        Image.fromarray((mask * 255).astype(np.uint8)).save(
                            temporary / "masks" / f"{index:05d}.png"
                        )
                    samples.append(memory(runtime.device))
                    progress(
                        0.05 + 0.45 * len(timings) / clip.count,
                        f"Tracked {len(timings)}/{clip.count} frames",
                    )
            if len(timings) != clip.count:
                raise ValueError(
                    "Tracking did not return every frame. The incomplete cache was discarded."
                )
            runtime.predictor.reset_state(state)
            state = None
            previous = None
            deltas = []
            for index in range(clip.count):
                if cancelled():
                    raise Cancelled("Cancelled; no completed result was cached.")
                rgb = clip.frame(index)
                with Image.open(temporary / "masks" / f"{index:05d}.png") as image:
                    mask = np.asarray(image) > 0
                tick = time.perf_counter()
                if mode == "quality":
                    trimap = trimap_from_mask(mask, band, brushes.get(index))
                    alpha, tiles = refine(rgb, trimap, runtime.infer_tile, cancelled)
                    Image.fromarray(trimap).save(
                        temporary / "trimaps" / f"{index:05d}.png"
                    )
                else:
                    alpha, tiles = mask.astype(np.float32), 0
                sync(runtime.device)
                timings[index].update(
                    refinement_ms=(time.perf_counter() - tick) * 1000,
                    matte_tiles=tiles,
                    foreground_fraction=float(mask.mean()),
                    soft_alpha_pixels=int(np.count_nonzero((alpha > 0) & (alpha < 1))),
                )
                if previous is not None:
                    deltas.append(float(np.abs(alpha - previous).mean()))
                previous = alpha
                write_frame(temporary, index, rgb, mask, alpha)
                samples.append(memory(runtime.device))
                progress(
                    0.5 + 0.4 * (index + 1) / clip.count,
                    f"Saved {index + 1}/{clip.count} {mode} frames",
                )
        inference_ms = (time.perf_counter() - start) * 1000
        progress(0.92, "Encoding previews and lossless PNG archive")
        previews(temporary, clip.fps)
        frame_metrics = [timings[i] for i in range(clip.count)]
        result = {
            "settings": settings,
            "cache_hit": False,
            "device": runtime.device,
            "source_dimensions": [clip.source_width, clip.source_height],
            "frames": clip.count,
            "model_setup_ms": runtime.model_setup_ms,
            "frame_setup_ms": frame_setup_ms,
            "prompt_ms": prompt_ms,
            "inference_and_png_ms": inference_ms,
            "tracking_mean_ms": float(
                np.mean([x["tracking_ms"] for x in frame_metrics])
            ),
            "refinement_mean_ms": float(
                np.mean([x["refinement_ms"] for x in frame_metrics])
            ),
            "raw_alpha_frame_difference_mean": float(np.mean(deltas))
            if deltas
            else None,
            "difference_note": "Uncompensated motion plus alpha change; not a flicker or accuracy score.",
            "memory_snapshot_max": {
                k: max(s.get(k, 0) for s in samples) for k in samples[0]
            },
            "memory_note": "Sampled snapshots; MPS transient allocation peak is not measured. RSS is process lifetime.",
            "per_frame": frame_metrics,
            "models": {
                "edgetam_parameters": sum(
                    p.numel() for p in runtime.predictor.parameters()
                ),
                "vitmatte_parameters": sum(
                    p.numel() for p in runtime.matte.parameters()
                )
                if mode == "quality"
                else None,
            },
        }
        (temporary / "benchmark.json").write_text(
            json.dumps(result, indent=2), encoding="utf-8"
        )
        annotate_outputs(temporary, clip)
        with ZipFile(temporary / "alpha-and-cutouts.zip", "w", ZIP_DEFLATED) as archive:
            archive.write(temporary / "benchmark.json", "benchmark.json")
            for folder in ["alpha", "rgba", "trimaps"]:
                for file in (temporary / folder).iterdir():
                    archive.write(file, str(file.relative_to(temporary)))
        annotate_outputs(temporary, clip)
        if cancelled():
            raise Cancelled("Cancelled; no completed result was cached.")
        if destination.exists():
            shutil.rmtree(destination)
        temporary.replace(destination)
        progress(1.0, "Completed")
        return destination, result
    finally:
        if state is not None and runtime.predictor is not None:
            runtime.predictor.reset_state(state)
        if temporary.exists():
            shutil.rmtree(temporary)
