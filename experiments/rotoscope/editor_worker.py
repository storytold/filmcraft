"""Loopback-only model worker for the feature-gated native editor experiment.

FilmCraft decodes and sends bounded PPM frames; this process runs only model inference.
No ffmpeg/ffprobe, source-file decoding or editor project mutation occurs here.
"""

import argparse
import hashlib
import json
import tempfile
import threading
import time
import traceback
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import numpy as np
import torch
from inference_cache import BudgetLRU, state_bytes
from matting import Cancelled, refine, trimap_from_mask
from models import DATA, EDGE_SHA, MATTE_SHA
from PIL import Image
from pipeline import PIPELINE_VERSION, Runtime

JOBS = {}
LOCK = threading.Lock()
RUNTIME = Runtime("auto", refinement="auto")
INFERENCE = threading.Lock()
FEATURES = BudgetLRU(2, 192 * 1024 * 1024)
TRACKED = BudgetLRU(4, 40_000_000)
ALPHAS = BudgetLRU(120, 80_000_000)


def validate(data):
    if data.get("version") != 1:
        raise ValueError("Unsupported editor protocol version")
    directory = Path(data["directory"]).resolve()
    if directory.parent != Path(
        tempfile.gettempdir()
    ).resolve() or not directory.name.startswith("filmcraft-ai-"):
        raise ValueError("Only this editor's temporary frame directories are accepted")
    width, height = data["width"], data["height"]
    times = data["times"]
    if (
        not all(type(x) is int for x in (width, height))
        or min(width, height) < 1
        or width * height > 4_194_304
    ):
        raise ValueError("Invalid model frame dimensions")
    if not 1 <= len(times) <= 120 or width * height * len(times) > 40_000_000:
        raise ValueError("Editor input exceeds the frame/pixel budget")
    if (
        not all(type(t) is int for t in times)
        or times != sorted(set(times))
        or times[-1] >= data["end"]
    ):
        raise ValueError("Invalid source frame times")
    if (
        type(data["quality"]) is not bool
        or not 1 <= data["band"] <= 32
        or len(data["points"]) > 512
    ):
        raise ValueError("Invalid processing settings")
    for point in data["points"]:
        if (
            point["kind"] not in {"foreground", "background", "refine"}
            or not np.isfinite([point["x"], point["y"]]).all()
        ):
            raise ValueError("Invalid object point")
        if (
            not 0 <= point["x"] < width
            or not 0 <= point["y"] < height
            or not times[0] <= point["time"] < data["end"]
        ):
            raise ValueError("Object point is outside the processed frames")
    return directory


def encode(alpha):
    values = np.rint(np.clip(alpha, 0, 1) * 65535).astype(np.uint16).ravel()
    return encode_values(values)


def encode_values(values):
    values = values.ravel()
    cuts = np.r_[0, np.flatnonzero(values[1:] != values[:-1]) + 1, len(values)]
    return np.column_stack([np.diff(cuts), values[cuts[:-1]]]).tolist()


def refinement_key(rgb, trimap):
    digest = hashlib.sha256(
        f"{MATTE_SHA}:{PIPELINE_VERSION}:{RUNTIME.matte_device}:{rgb.shape}".encode()
    )
    digest.update(rgb.tobytes())
    digest.update(trimap.tobytes())
    return digest.hexdigest()


def result_key(data, frames_digest):
    return hashlib.sha256(
        json.dumps(
            {
                **data,
                "directory": None,
                "pipeline": PIPELINE_VERSION,
                "editor_cache": 4,
                "device": RUNTIME.device,
                "refinement_device": RUNTIME.matte_device if data["quality"] else None,
                "edge": EDGE_SHA,
                "matte": MATTE_SHA,
                "frames_sha256": frames_digest,
            },
            sort_keys=True,
        ).encode()
    ).hexdigest()


def infer(job, data, directory):
    # Cached state is reset before releasing the inference lock, including cancellation.
    with INFERENCE, torch.inference_mode():
        infer_locked(job, data, directory)


def infer_locked(job, data, directory):
    state = None
    feature_key = None
    started = time.perf_counter()
    metrics = {
        "features_cache_hit": False,
        "tracking_cache_hit": False,
        "alpha_cache_hit": False,
        "alpha_frames_reused": 0,
        "refinement_device": RUNTIME.matte_device,
    }
    try:
        if job["cancel"].is_set():
            raise Cancelled("Cancelled")
        times = np.asarray(data["times"], dtype=np.int64)
        points = data["points"]
        frames = len(times)
        frame_digest = hashlib.sha256(
            f"{data['width']}x{data['height']}:{frames}".encode()
        )
        for i in range(frames):
            if job["cancel"].is_set():
                raise Cancelled("Cancelled")
            path = directory / f"{i:05d}.ppm"
            if path.stat().st_size > data["width"] * data["height"] * 3 + 128:
                raise ValueError("Oversized input frame")
            with Image.open(path) as image:
                if image.size != (data["width"], data["height"]):
                    raise ValueError("Input frame dimensions differ")
                rgb = image.convert("RGB")
                pixels = rgb.tobytes()
                frame_digest.update(pixels)
                rgb.save(directory / f"{i:05d}.jpg", quality=95)
        key = result_key(data, frame_digest.hexdigest())
        metrics["input_ms"] = (time.perf_counter() - started) * 1000
        cache = DATA / "editor-cache" / f"{key}.json"
        if cache.exists():
            metrics["alpha_cache_hit"] = True
            with LOCK:
                job.update(
                    mask=json.loads(cache.read_text()),
                    done=frames * 2,
                    status="Loaded cached alpha",
                )
            return
        with LOCK:
            job["status"] = "Loading models"
        models_start = time.perf_counter()
        RUNTIME.prepare(quality=data["quality"])
        metrics["models_ms"] = (time.perf_counter() - models_start) * 1000
        if job["cancel"].is_set():
            raise Cancelled("Cancelled")
        prompts = {}
        brushes = {}
        for point in points:
            i = int(np.searchsorted(times, point["time"], side="right") - 1)
            if point["kind"] == "refine":
                brush = brushes.setdefault(
                    i, np.zeros((data["height"], data["width"]), bool)
                )
                yy, xx = np.ogrid[: data["height"], : data["width"]]
                brush |= (xx - point["x"]) ** 2 + (yy - point["y"]) ** 2 <= (
                    data["band"] * 2
                ) ** 2
            else:
                prompts.setdefault(i, []).append(
                    [point["x"], point["y"], int(point["kind"] == "foreground")]
                )
        if not any(p[2] for group in prompts.values() for p in group):
            raise ValueError("Add a foreground point")
        tracking_key = hashlib.sha256(
            json.dumps(
                {
                    "image": frame_digest.hexdigest(),
                    "edge": EDGE_SHA,
                    "device": RUNTIME.device,
                    "pipeline": PIPELINE_VERSION,
                    "prompts": sorted(prompts.items()),
                },
                sort_keys=True,
            ).encode()
        ).hexdigest()
        tracking_start = time.perf_counter()
        masks = TRACKED.get(tracking_key)
        if masks is not None:
            metrics["tracking_cache_hit"] = True
            with LOCK:
                job.update(done=frames, status="Reusing tracked masks")
        else:
            if frames == 1 and RUNTIME.device == "cpu":
                feature_key = frame_digest.hexdigest()
                state = FEATURES.get(feature_key)
                metrics["features_cache_hit"] = state is not None
            if state is None:
                state = RUNTIME.predictor.init_state(
                    str(directory),
                    offload_video_to_cpu=True,
                    offload_state_to_cpu=True,
                    async_loading_frames=False,
                )
            for i, group in sorted(prompts.items()):
                RUNTIME.predictor.add_new_points_or_box(
                    state,
                    frame_idx=i,
                    obj_id=1,
                    points=np.asarray([p[:2] for p in group], np.float32),
                    labels=np.asarray([p[2] for p in group], np.int32),
                )
            seed = min(prompts)
            masks = {}
            for reverse in [False, True] if seed else [False]:
                for i, ids, logits in RUNTIME.predictor.propagate_in_video(
                    state, start_frame_idx=seed, reverse=reverse
                ):
                    if job["cancel"].is_set():
                        raise Cancelled("Cancelled")
                    mask = (logits[ids.index(1), 0] > 0).cpu().numpy().copy()
                    if mask.shape != (data["height"], data["width"]):
                        raise ValueError("Tracking returned invalid mask dimensions")
                    mask.setflags(write=False)
                    masks[i] = mask
                    with LOCK:
                        job.update(
                            done=len(masks), status=f"Tracking {len(masks)}/{frames}"
                        )
            if set(masks) != set(range(frames)):
                raise ValueError("Tracking did not produce every requested frame")
            TRACKED.put(tracking_key, masks, sum(m.nbytes for m in masks.values()))
        metrics["tracking_ms"] = (time.perf_counter() - tracking_start) * 1000
        if state is not None:
            RUNTIME.predictor.reset_state(state)
            if feature_key is not None:
                FEATURES.put(feature_key, state, state_bytes(state))
            # Full-clip frame tensors can be large; release them before matting.
            state = None
        refinement_start = time.perf_counter()
        # Auto Metal failures restart the entire refinement pass on CPU, so a saved
        # result never mixes devices or receives the wrong cache identity.
        for _attempt in range(2):
            backend = RUNTIME.matte_device
            result, pending = [], []
            metrics["alpha_frames_reused"] = 0
            for i in range(frames):
                if job["cancel"].is_set():
                    raise Cancelled("Cancelled")
                if data["quality"]:
                    with Image.open(directory / f"{i:05d}.jpg") as image:
                        rgb = np.asarray(image.convert("RGB"))
                    trimap = trimap_from_mask(masks[i], data["band"], brushes.get(i))
                    alpha_key = refinement_key(rgb, trimap)
                    values = ALPHAS.get(alpha_key)
                    if values is None:
                        alpha, _ = refine(
                            rgb, trimap, RUNTIME.infer_tile, job["cancel"].is_set
                        )
                        if RUNTIME.matte_device != backend:
                            with LOCK:
                                job["status"] = "Retrying Quality refinement on CPU"
                            break
                        if not np.isfinite(alpha).all():
                            raise ValueError("Model produced invalid alpha")
                        values = np.rint(np.clip(alpha, 0, 1) * 65535).astype(np.uint16)
                        values.setflags(write=False)
                        pending.append((alpha_key, values))
                    else:
                        metrics["alpha_frames_reused"] += 1
                else:
                    values = masks[i].astype(np.uint16) * 65535
                result.append({"time": int(times[i]), "runs": encode_values(values)})
                with LOCK:
                    job.update(
                        done=frames + i + 1,
                        status=f"Refining {i + 1}/{frames}"
                        if data["quality"]
                        else f"Saving mask {i + 1}/{frames}",
                    )
            else:
                break
        else:
            raise ValueError(
                "Refinement backend changed repeatedly; select CPU and retry"
            )
        metrics["refinement_device"] = RUNTIME.matte_device
        metrics["refinement_ms"] = (time.perf_counter() - refinement_start) * 1000
        mask = {
            "width": data["width"],
            "height": data["height"],
            "end": data["end"],
            "quality": data["quality"],
            "frames": result,
        }
        if job["cancel"].is_set():
            raise Cancelled("Cancelled")
        cache = (
            DATA / "editor-cache" / f"{result_key(data, frame_digest.hexdigest())}.json"
        )
        cache.parent.mkdir(parents=True, exist_ok=True)
        temporary = cache.with_suffix(".tmp")
        temporary.write_text(json.dumps(mask, separators=(",", ":")))
        with LOCK:
            if job["cancel"].is_set():
                temporary.unlink(missing_ok=True)
                raise Cancelled("Cancelled")
            temporary.replace(cache)
            for alpha_key, values in pending:
                ALPHAS.put(alpha_key, values, values.nbytes)
            job.update(mask=mask, status="Complete")
    except Exception as error:  # noqa: BLE001 -- report model failures without abandoning the native job
        reported = Cancelled("Cancelled") if job["cancel"].is_set() else error
        if not isinstance(reported, Cancelled):
            traceback.print_exc()
        with LOCK:
            job["error"] = str(reported) or type(reported).__name__
    finally:
        if state is not None:
            try:
                RUNTIME.predictor.reset_state(state)
                if feature_key is not None:
                    FEATURES.put(feature_key, state, state_bytes(state))
            except Exception:  # noqa: BLE001 -- failed state cleanup must never poison reuse
                if feature_key is not None:
                    FEATURES.discard(feature_key)
                traceback.print_exc()
        metrics["total_ms"] = (time.perf_counter() - started) * 1000
        with LOCK:
            job["metrics"] = metrics


class Handler(BaseHTTPRequestHandler):
    def reply(self, data, code=200):
        payload = json.dumps(data, separators=(",", ":")).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        if self.path == "/health":
            self.reply(
                {
                    "application": "filmcraft-ai-reference",
                    "ready": True,
                    "device": RUNTIME.device,
                    "refinementDevice": RUNTIME.matte_device,
                    "version": 1,
                    "modelsReady": RUNTIME.predictor is not None
                    and RUNTIME.matte is not None,
                }
            )
            return
        with LOCK:
            job = JOBS.get(self.path.removeprefix("/jobs/"))
            data = {k: v for k, v in job.items() if k != "cancel"} if job else None
        self.reply(data or {"error": "Unknown job"}, 200 if data else 404)

    def do_POST(self):
        try:
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 <= size <= 128 * 1024:
                raise ValueError("Request exceeds the 128 KiB control-message budget")
            data = json.loads(self.rfile.read(size) or b"{}")
            if self.path.endswith("/cancel"):
                with LOCK:
                    job = JOBS.get(
                        self.path.removeprefix("/jobs/").removesuffix("/cancel")
                    )
                    if job:
                        job["cancel"].set()
                self.reply({"cancelled": bool(job)})
                return
            if self.path != "/jobs":
                self.reply({"error": "Unknown endpoint"}, 404)
                return
            directory = validate(data)
            identity = uuid.uuid4().hex
            job = {"cancel": threading.Event(), "status": "Queued", "done": 0}
            with LOCK:
                if len(JOBS) >= 16:
                    expired = next(
                        (
                            key
                            for key, value in JOBS.items()
                            if "mask" in value or "error" in value
                        ),
                        None,
                    )
                    if expired is None:
                        raise ValueError("The worker queue is full")
                    del JOBS[expired]
                JOBS[identity] = job
            threading.Thread(
                target=infer, args=(job, data, directory), daemon=True
            ).start()
            self.reply({"id": identity})
        except Exception as error:  # noqa: BLE001 -- malformed IPC requests must receive a JSON error
            self.reply({"error": str(error)}, 400)

    def log_message(self, *_args):
        pass


def warm_models():
    try:
        with INFERENCE, torch.inference_mode():
            RUNTIME.prepare(quality=True)
    except Exception:  # noqa: BLE001 -- startup warmup must leave the worker available to report job errors
        traceback.print_exc()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=7871)
    parser.add_argument(
        "--warmup",
        action="store_true",
        help="Preload both models while the editor opens",
    )
    parser.add_argument(
        "--refinement-device",
        choices=["auto", "cpu", "mps", "cuda"],
        default="auto",
        help="Quality backend; auto uses Metal refinement with CPU tracking on this Mac",
    )
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error("Port must be 1024..65535")
    RUNTIME = Runtime("auto", refinement=args.refinement_device)
    print(
        f"Editor model worker: http://127.0.0.1:{args.port} ({RUNTIME.device})",
        flush=True,
    )
    if args.warmup:
        threading.Thread(target=warm_models, daemon=True).start()
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()
