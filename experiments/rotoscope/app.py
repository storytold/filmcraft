"""Local interactive reference test using stock Gradio controls."""

import argparse
import logging
import threading
from functools import wraps

import gradio as gr
import numpy as np
from matting import Cancelled
from media import composite, load_clip, read_alpha
from models import DATA, DOG_PAGE, sample
from PIL import Image, ImageDraw
from pipeline import Runtime, run

LOG = logging.getLogger("rotoscope")
RUNTIMES = {}
JOB_LOCK = threading.Lock()


class Session:
    def __init__(self):
        self.clip = None
        self.prompts = {}
        self.brushes = {}
        self.output = None
        self.metrics = None
        self.cancel = threading.Event()
        self.busy = False

    def __deepcopy__(self, memo):
        # Gradio deep-copies the initial state per browser session. Thread events
        # cannot be pickled; every new browser receives its own empty session.
        return Session()


def friendly(function):
    @wraps(function)
    def wrapper(*args, **kwargs):
        try:
            return function(*args, **kwargs)
        except gr.Error:
            raise
        except Exception as error:
            LOG.exception("Reference experiment failed")
            raise gr.Error(
                str(error) or "The operation failed; see the local terminal log."
            ) from error

    return wrapper


def ensure_idle(session):
    if session.busy:
        raise ValueError(
            "Processing is running. Cancel it or wait before changing this clip."
        )


def selected_frame(session, index):
    if session.clip is None:
        raise ValueError("Load a clip first.")
    return session.clip.frame(int(index))


def marked_frame(session, index):
    image = Image.fromarray(selected_frame(session, index))
    draw = ImageDraw.Draw(image)
    for x, y, label in session.prompts.get(int(index), []):
        color = "#49d4ad" if label else "#ff7878"
        draw.ellipse((x - 6, y - 6, x + 6, y + 6), fill=color, outline="white", width=2)
    return np.asarray(image)


def description(session):
    if session.clip is None:
        return "Load a clip to begin."
    clip = session.clip
    points = sum(len(v) for v in session.prompts.values())
    text = f"Loaded **{clip.count} frames**, **{clip.width} × {clip.height}**, **{clip.fps} fps**. {points} points on {len(session.prompts)} frames."
    if session.output is None:
        return text + " Add a foreground point, then track."
    return (
        text
        + f" Completed **{session.metrics['device'].upper()}** inference. Scrub below or watch the previews."
    )


@friendly
def load(path, max_frames, max_side, session):
    ensure_idle(session)
    if session.cancel.is_set():
        session.cancel.clear()
    clip = load_clip(path, int(max_frames), int(max_side))
    session.clip, session.prompts, session.brushes = clip, {}, {}
    session.output, session.metrics = None, None
    frame = clip.frame(0)
    return (
        session,
        gr.update(
            value=0,
            minimum=0,
            maximum=max(1, clip.count - 1),
            interactive=clip.count > 1,
        ),
        frame,
        {"background": frame, "layers": [], "composite": frame},
        None,
        None,
        None,
        None,
        None,
        None,
        description(session),
    )


@friendly
def seek(index, session):
    ensure_idle(session)
    rgb = selected_frame(session, index)
    view = marked_frame(session, index)
    cutout = alpha = None
    if session.output is not None:
        alpha = read_alpha(session.output / "alpha" / f"{int(index):05d}.png")
        cutout = composite(rgb, alpha)
    return (
        view,
        {"background": rgb, "layers": [], "composite": rgb},
        cutout,
        alpha,
        description(session),
    )


@friendly
def click(index, kind, session, event: gr.SelectData):
    ensure_idle(session)
    rgb = selected_frame(session, index)
    if not isinstance(event.index, (list, tuple)) or len(event.index) != 2:
        raise ValueError("Click inside the image to select a point.")
    x, y = event.index
    if not 0 <= x < rgb.shape[1] or not 0 <= y < rgb.shape[0]:
        raise ValueError("Point lies outside the loaded image.")
    points = session.prompts.setdefault(int(index), [])
    if len(points) >= 64:
        raise ValueError("Use at most 64 points on one frame.")
    points.append([float(x), float(y), 1 if kind == "Foreground" else 0])
    session.output = session.metrics = None
    return (
        marked_frame(session, index),
        description(session),
        session,
        None,
        None,
        None,
        None,
        None,
        None,
    )


@friendly
def clear(index, session):
    ensure_idle(session)
    session.prompts.pop(int(index), None)
    session.brushes.pop(int(index), None)
    session.output = session.metrics = None
    return (
        marked_frame(session, index),
        description(session),
        session,
        None,
        None,
        None,
        None,
        None,
        None,
    )


@friendly
def save_brush(editor, index, session):
    ensure_idle(session)
    selected_frame(session, index)
    painted = np.zeros((session.clip.height, session.clip.width), dtype=bool)
    for layer in (editor or {}).get("layers", []):
        layer = np.asarray(layer)
        if layer.shape[:2] != painted.shape or layer.ndim != 3 or layer.shape[2] != 4:
            raise ValueError(
                "Brush layer must have the loaded image's dimensions and an alpha channel."
            )
        painted |= layer[..., 3] > 0
    if not painted.any():
        raise ValueError("Paint the uncertain edge region first.")
    session.brushes[int(index)] = painted
    session.output = session.metrics = None
    return (
        f"Saved a Refine Edge region of {int(painted.sum())} pixels on frame {int(index)}. Select Quality and track again.",
        session,
        None,
        None,
        None,
        None,
        None,
        None,
    )


GRADIO_PROGRESS = gr.Progress()


@friendly
def track(mode, device, band, index, session, progress=GRADIO_PROGRESS):
    if session.clip is None:
        raise ValueError("Load a clip first.")
    if not JOB_LOCK.acquire(blocking=False):
        raise ValueError(
            "Another local inference job is running. Wait for it or cancel it first."
        )
    try:
        session.busy = True
        session.cancel.clear()
        if device not in RUNTIMES:
            RUNTIMES[device] = Runtime(device)
        directory, metrics = run(
            session.clip,
            [{"frame": i, "points": p} for i, p in session.prompts.items()],
            RUNTIMES[device],
            mode=mode.lower(),
            band=int(band),
            brushes=session.brushes,
            cancelled=session.cancel.is_set,
            progress=lambda value, message: progress(value, desc=message),
        )
        session.output, session.metrics = directory, metrics
        rgb = selected_frame(session, index)
        alpha = read_alpha(directory / "alpha" / f"{int(index):05d}.png")
        summary = {
            "mode": mode,
            "device": metrics["device"],
            "frames": metrics["frames"],
            "cache_hit": metrics["cache_hit"],
            "tracking_mean_ms": round(metrics["tracking_mean_ms"], 2),
            "refinement_mean_ms": round(metrics["refinement_mean_ms"], 2),
            "model_setup_ms": round(metrics["model_setup_ms"], 2),
            "memory_snapshot_max": metrics["memory_snapshot_max"],
            "memory_note": metrics["memory_note"],
            "output": str(directory),
        }
        return (
            composite(rgb, alpha),
            alpha,
            str(directory / "overlay.mp4"),
            str(directory / "preview.mp4"),
            str(directory / "alpha-and-cutouts.zip"),
            summary,
            description(session),
            session,
        )
    except Cancelled:
        return (
            *[gr.skip()] * 6,
            "Cancelled. The incomplete result was discarded; existing cached results remain available.",
            session,
        )
    finally:
        session.busy = False
        JOB_LOCK.release()


def cancel(session):
    session.cancel.set()
    return "Cancellation requested. The current model step will finish before the job stops."


@friendly
def invalidate(session):
    ensure_idle(session)
    session.clip, session.output, session.metrics = None, None, None
    session.prompts, session.brushes = {}, {}
    return (
        session,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        "Settings changed. Select Load clip to decode with the new limits.",
    )


def build():
    with gr.Blocks(
        title="FilmCraft · Rotoscope test", theme=gr.themes.Base(primary_hue="teal")
    ) as app:
        session = gr.State(Session())
        gr.Markdown(
            "# FilmCraft · Rotoscope test\nLocal test of EdgeTAM tracking + ViTMatte edge refinement · [Issue #1733](https://github.com/storytold/filmcraft/issues/1733)"
        )
        with gr.Row():
            upload = gr.Video(
                value=str(sample()),
                label="Source clip",
                sources=["upload"],
                interactive=True,
            )
            with gr.Column():
                max_frames = gr.Slider(
                    1, 120, value=30, step=1, label="Frame limit (from start of clip)"
                )
                max_side = gr.Dropdown(
                    [512, 720, 1080, 1920, 3840],
                    value=720,
                    label="Longest side (pixels)",
                )
                load_button = gr.Button(
                    "Load clip", variant="primary", elem_id="load-clip"
                )
                mode = gr.Radio(
                    ["Fast", "Quality"], value="Fast", label="Processing mode"
                )
                device = gr.Dropdown(
                    ["auto", "mps", "cuda", "cpu"],
                    value="auto",
                    label="Inference device",
                )
                band = gr.Slider(
                    1, 32, value=8, step=1, label="Quality edge band (pixels)"
                )
        status = gr.Markdown("Load the sample, then click the dog to select it.")
        slider = gr.Slider(
            0, 1, value=0, step=1, label="Frame (scrub cached masks after tracking)"
        )
        with gr.Row():
            with gr.Column():
                kind = gr.Radio(
                    ["Foreground", "Background"], value="Foreground", label="Click type"
                )
                selection = gr.Image(
                    label="Click to select / correct this frame",
                    type="numpy",
                    interactive=False,
                    elem_id="point-image",
                )
                clear_button = gr.Button("Clear prompts on this frame")
            cutout = gr.Image(
                label="Cutout on checkerboard",
                type="numpy",
                interactive=False,
                elem_id="cutout-image",
            )
            alpha_view = gr.Image(
                label="Alpha matte",
                type="numpy",
                image_mode="L",
                interactive=False,
                elem_id="alpha-image",
            )
        with gr.Row():
            track_button = gr.Button(
                "Track clip", variant="primary", elem_id="track-clip"
            )
            cancel_button = gr.Button("Cancel processing", elem_id="cancel-job")
        with gr.Accordion("Refine Edge brush (Quality mode)", open=False):
            gr.Markdown(
                "Paint uncertain boundaries, save the brush, then track in Quality mode. Brush regions apply only to their painted frame in this experiment."
            )
            editor = gr.ImageEditor(
                label="Paint an uncertain edge region",
                type="numpy",
                image_mode="RGBA",
                brush=gr.Brush(colors=["#f59e0b"], color_mode="fixed"),
                transforms=[],
                layers=False,
                elem_id="refine-editor",
            )
            brush_button = gr.Button("Save brush on this frame")
        with gr.Row():
            overlay_video = gr.Video(label="Tracked mask overlay", interactive=False)
            cutout_video = gr.Video(label="Cutout preview", interactive=False)
        archive = gr.File(
            label="Lossless 16-bit alpha + transparent PNGs + benchmark",
            interactive=False,
        )
        metrics_view = gr.JSON(label="Measured performance")
        gr.Markdown(
            f"Sample: [Sleeping dog]({DOG_PAGE}) by Amada44 · [CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/) · previews are trimmed/scaled/masked derivatives.\n\nThis is a standalone model experiment. Frame-by-frame matting has no temporal smoothing yet; the numbers here do not prove real-time playback or production 4K quality."
        )

        load_button.click(
            load,
            [upload, max_frames, max_side, session],
            [
                session,
                slider,
                selection,
                editor,
                cutout,
                alpha_view,
                overlay_video,
                cutout_video,
                archive,
                metrics_view,
                status,
            ],
            api_name="load_clip",
        )
        for control in [max_frames, max_side]:
            control.change(
                invalidate,
                [session],
                [
                    session,
                    selection,
                    editor,
                    cutout,
                    alpha_view,
                    overlay_video,
                    cutout_video,
                    archive,
                    metrics_view,
                    status,
                ],
                api_name=False,
            )
        slider.change(
            seek,
            [slider, session],
            [selection, editor, cutout, alpha_view, status],
            api_name="seek",
        )
        selection.select(
            click,
            [slider, kind, session],
            [
                selection,
                status,
                session,
                cutout,
                alpha_view,
                overlay_video,
                cutout_video,
                archive,
                metrics_view,
            ],
            api_name=False,
        )
        clear_button.click(
            clear,
            [slider, session],
            [
                selection,
                status,
                session,
                cutout,
                alpha_view,
                overlay_video,
                cutout_video,
                archive,
                metrics_view,
            ],
            api_name="clear_prompts",
        )
        brush_button.click(
            save_brush,
            [editor, slider, session],
            [
                status,
                session,
                cutout,
                alpha_view,
                overlay_video,
                cutout_video,
                archive,
                metrics_view,
            ],
            api_name="save_brush",
        )
        track_button.click(
            track,
            [mode, device, band, slider, session],
            [
                cutout,
                alpha_view,
                overlay_video,
                cutout_video,
                archive,
                metrics_view,
                status,
                session,
            ],
            api_name="track",
            concurrency_id="inference",
            concurrency_limit=1,
        )
        cancel_button.click(cancel, [session], [status], queue=False, api_name="cancel")
    return app


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=7860)
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error("Port must be from 1024 to 65535.")
    DATA.mkdir(parents=True, exist_ok=True)
    gr.set_static_paths([])
    build().queue().launch(
        server_name="127.0.0.1",
        server_port=args.port,
        share=False,
        allowed_paths=[str(DATA)],
        show_error=True,
    )
