"""Offline export of the pinned models for FilmCraft's pure-Rust ONNX runtime.

Python is a preparation/oracle tool only; exported graphs contain the actual model
weights and execute without Python, PyTorch or an inference server in the editor.
"""

import argparse
import hashlib
import json
import types
from pathlib import Path

import torch
from models import (
    DATA,
    EDGE_REV,
    EDGE_SHA,
    MATTE_REV,
    MATTE_SHA,
    attribution,
    model_paths,
)
from torch import nn
from torch.nn import functional as F
from sam2.modeling.sam.transformer import RoPEAttentionv2

FORMAT_VERSION = 1
OPSET = 23


class RealRotaryAttention(RoPEAttentionv2):
    """Equivalent real-valued rotation; ONNX does not represent complex tensors."""

    def __init__(self, original, spatial=False):
        nn.Module.__init__(self)
        self.q_proj, self.k_proj = original.q_proj, original.k_proj
        self.v_proj, self.out_proj = original.v_proj, original.out_proj
        self.heads = original.num_heads
        self.spatial = spatial
        q_frequency = original.compute_cis(end_x=64, end_y=64)
        k_frequency = original.freqs_cis_k if spatial else q_frequency
        for prefix, value in [("q", q_frequency), ("k", k_frequency)]:
            self.register_buffer(prefix + "_cos", value.real.clone())
            self.register_buffer(prefix + "_sin", value.imag.clone())

    @staticmethod
    def rotate(value, cosine, sine):
        even, odd = value[..., 0::2], value[..., 1::2]
        return torch.stack(
            [even * cosine - odd * sine, even * sine + odd * cosine], dim=-1
        ).flatten(-2)

    def forward(self, q, k, v, num_k_exclude_rope=0, rope_k_repeat=-1):
        def heads(projection, value):
            value = projection(value)
            return value.reshape(
                value.shape[0], -1, self.heads, value.shape[-1] // self.heads
            ).transpose(1, 2)

        q, k, v = heads(self.q_proj, q), heads(self.k_proj, k), heads(self.v_proj, v)
        q = self.rotate(q, self.q_cos, self.q_sin)
        if self.spatial:
            spatial_length = rope_k_repeat * 512
            bank = k[:, :, :spatial_length].reshape(
                1, self.heads, rope_k_repeat, 512, -1
            )
            # EdgeTAM's first 256 perceiver latents are global, followed by a 16² grid.
            bank = torch.cat(
                [
                    bank[:, :, :, :256],
                    self.rotate(bank[:, :, :, 256:], self.k_cos, self.k_sin),
                ],
                dim=3,
            ).reshape(1, self.heads, spatial_length, -1)
            k = torch.cat([bank, k[:, :, spatial_length:]], dim=2)
        else:
            k = self.rotate(k, self.k_cos, self.k_sin)
        output = F.scaled_dot_product_attention(q, k, v, dropout_p=0.0)
        output = output.transpose(1, 2).reshape(
            1, -1, output.shape[1] * output.shape[-1]
        )
        return self.out_proj(output)


class ImageEncoder(nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, image):
        return tuple(self.model.forward_image(image)["backbone_fpn"])


class Decoder(nn.Module):
    def __init__(self, model, position, tracking=False, multiple=False):
        super().__init__()
        self.model = model
        self.tracking, self.multiple = tracking, multiple
        self.register_buffer("position", position)

    def forward(self, high0, high1, image, a, b, pointers=None):
        if self.tracking:
            memories, positions = a, b
            n = memories.shape[0]
            ptrs = pointers.reshape(-1, 64).unsqueeze(1)
            memory = torch.cat([memories.reshape(-1, 64).unsqueeze(1), ptrs], dim=0)
            memory_position = torch.cat(
                [positions.reshape(-1, 64).unsqueeze(1), torch.zeros_like(ptrs)], dim=0
            )
            attention = self.model.memory_attention
            features = image.flatten(2).permute(2, 0, 1)
            if attention.pos_enc_at_input:
                features = features + 0.1 * self.position
            query_position = self.position
            if attention.batch_first:
                features, query_position = (
                    features.transpose(0, 1),
                    query_position.transpose(0, 1),
                )
                memory, memory_position = (
                    memory.transpose(0, 1),
                    memory_position.transpose(0, 1),
                )
            for layer in attention.layers:
                features = layer(
                    tgt=features,
                    memory=memory,
                    query_pos=query_position,
                    pos=memory_position,
                    num_k_exclude_rope=ptrs.shape[0],
                    rope_k_repeat=n,
                )
            features = attention.norm(features)
            if attention.batch_first:
                features = features.transpose(0, 1)
            features = features.permute(1, 2, 0).reshape(1, 256, 64, 64)
            points = None
        else:
            features = image + self.model.no_mem_embed.reshape(1, 256, 1, 1)
            points = {"point_coords": a, "point_labels": b}
        output = self.model._forward_sam_heads(
            features,
            point_inputs=points,
            high_res_features=[high0, high1],
            multimask_output=not self.multiple,
        )
        return output[3], output[5], output[6]


class MemoryEncoder(nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, image, mask):
        mask = F.interpolate(
            mask, size=(1024, 1024), mode="bilinear", align_corners=False
        )
        features, position = self.model._encode_new_memory(
            [image.flatten(2).permute(2, 0, 1)],
            [(64, 64)],
            mask,
            torch.ones(1, 1),
            True,
        )
        return features, position[0], self.model.maskmem_tpos_enc.reshape(7, 64)


class Matte(nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, pixels, position):
        embedding = self.model.backbone.embeddings
        original = embedding.get_absolute_positions
        embedding.get_absolute_positions = lambda _base, _cls, _h, _w: position
        try:
            alpha = self.model(pixel_values=pixels).alphas
        finally:
            embedding.get_absolute_positions = original
        return alpha, embedding.position_embeddings


def dynamic_relative_position(q_size, k_size, relative_position):
    # All ViTMatte backbone attention is self-attention: q_size == k_size.
    # Always resizing preserves a dynamic shape even at the checkpoint's native size.
    position = (
        F.interpolate(
            relative_position.T.unsqueeze(0),
            size=2 * q_size - 1,
            mode="linear",
            align_corners=False,
        )
        .squeeze(0)
        .T
    )
    distance = (
        torch.arange(q_size)[:, None] - torch.arange(k_size)[None, :] + k_size - 1
    )
    return position[distance.long()]


def export_point_embedding(self, points, labels, pad):
    points = points + 0.5
    if pad:
        points = torch.cat([points, torch.zeros(points.shape[0], 1, 2)], dim=1)
        labels = torch.cat(
            [labels, -torch.ones(labels.shape[0], 1, dtype=labels.dtype)], dim=1
        )
    embedding = self.pe_layer.forward_with_coords(points, self.input_image_size)
    label = labels.unsqueeze(-1)
    embedding = torch.where(label == -1, torch.zeros_like(embedding), embedding)
    embedding = (
        embedding + (label == -1).to(embedding.dtype) * self.not_a_point_embed.weight
    )
    for i in range(4):
        embedding = (
            embedding
            + (label == i).to(embedding.dtype) * self.point_embeddings[i].weight
        )
    return embedding


def export(destination):
    destination.mkdir(parents=True, exist_ok=True)
    # Keep the original CPU model-loading contract without the prototype UI/worker.
    from sam2.build_sam import build_sam2_video_predictor
    from transformers import VitMatteForImageMatting

    torch.set_num_threads(min(4, torch.get_num_threads()))
    edge_path, matte_path = model_paths(quality=True)
    model = build_sam2_video_predictor(
        "edgetam.yaml",
        str(edge_path),
        device="cpu",
        apply_postprocessing=False,
        hydra_overrides_extra=[
            "++model.fill_hole_area=0",
            "++model.sam_mask_decoder_extra_args.dynamic_multimask_via_stability=true",
        ],
    )
    matte = VitMatteForImageMatting.from_pretrained(
        str(matte_path), local_files_only=True, use_safetensors=True
    ).eval()
    from sam2.modeling.sam.transformer import RoPEAttention, RoPEAttentionv2
    from transformers.models.vitdet import modeling_vitdet

    with torch.inference_mode():
        image = torch.zeros(1, 3, 1024, 1024)
        backbone = model.forward_image(image)
        high0, high1, low = backbone["backbone_fpn"]
        position = backbone["vision_pos_enc"][-1].flatten(2).permute(2, 0, 1)
        for layer in model.memory_attention.layers:
            if not isinstance(layer.self_attn, RoPEAttention) or not isinstance(
                layer.cross_attn_image, RoPEAttentionv2
            ):
                raise ValueError("Pinned EdgeTAM attention architecture changed")
            layer.self_attn = RealRotaryAttention(layer.self_attn)
            layer.cross_attn_image = RealRotaryAttention(
                layer.cross_attn_image, spatial=True
            )
        model.sam_prompt_encoder._embed_points = types.MethodType(
            export_point_embedding, model.sam_prompt_encoder
        )
        if (
            model.no_obj_embed_spatial is not None
            or model.binarize_mask_from_pts_for_mem_enc
        ):
            raise ValueError("Pinned EdgeTAM memory configuration changed")
        modeling_vitdet.get_rel_pos = dynamic_relative_position
        coordinates = torch.tensor([[[512.0, 512.0]]])
        labels = torch.ones(1, 1, dtype=torch.int32)
        memories, positions = torch.zeros(2, 512, 64), torch.zeros(2, 512, 64)
        ptrs = torch.zeros(2, 256)
        entries = [
            (
                "image",
                ImageEncoder(model),
                (image,),
                ["image"],
                ["high0", "high1", "image_features"],
                {},
            ),
            (
                "prompt_one",
                Decoder(model, position),
                (high0, high1, low, coordinates, labels),
                ["high0", "high1", "image_features", "points", "labels"],
                ["mask", "pointer", "score"],
                {},
            ),
            (
                "prompt_many",
                Decoder(model, position, multiple=True),
                (high0, high1, low, coordinates.repeat(1, 2, 1), labels.repeat(1, 2)),
                ["high0", "high1", "image_features", "points", "labels"],
                ["mask", "pointer", "score"],
                {"points": {1: "points"}, "labels": {1: "points"}},
            ),
            (
                "track",
                Decoder(model, position, tracking=True),
                (high0, high1, low, memories, positions, ptrs),
                [
                    "high0",
                    "high1",
                    "image_features",
                    "memories",
                    "positions",
                    "pointers",
                ],
                ["mask", "pointer", "score"],
                {
                    "memories": {0: "memories"},
                    "positions": {0: "memories"},
                    "pointers": {0: "pointers"},
                },
            ),
            (
                "memory",
                MemoryEncoder(model),
                (low, torch.zeros(1, 1, 256, 256)),
                ["image_features", "mask"],
                ["memory", "position", "temporal"],
                {},
            ),
            (
                "matte",
                Matte(matte),
                (torch.zeros(1, 4, 448, 448), torch.zeros(1, 28, 28, 384)),
                ["pixels", "position"],
                ["alpha", "position_base"],
                {
                    "pixels": {2: "height", 3: "width"},
                    "position": {1: "token_height", 2: "token_width"},
                    "alpha": {2: "height", 3: "width"},
                },
            ),
        ]
        files = []
        for name, module, inputs, input_names, output_names, dynamic in entries:
            path = destination / f"{name}.onnx"
            temporary = path.with_suffix(".tmp")
            print(f"Exporting {name}", flush=True)
            torch.onnx.export(
                module.eval(),
                inputs,
                temporary,
                input_names=input_names,
                output_names=output_names,
                dynamic_axes=dynamic,
                opset_version=17,
                dynamo=False,
                external_data=False,
            )
            temporary.replace(path)
            import onnx

            graph = onnx.load(path)
            graph = onnx.version_converter.convert_version(graph, OPSET)
            # Legacy PyTorch export can ignore output_names for folded constants.
            # Keep a stable public tensor contract for the Rust loader.
            for value, expected in zip(graph.graph.output, output_names, strict=True):
                old = value.name
                if old == expected:
                    continue
                for tensor in [
                    *graph.graph.input,
                    *graph.graph.output,
                    *graph.graph.value_info,
                    *graph.graph.initializer,
                ]:
                    if tensor.name == old:
                        tensor.name = expected
                for node in graph.graph.node:
                    for names in [node.input, node.output]:
                        for i, tensor_name in enumerate(names):
                            if tensor_name == old:
                                names[i] = expected
            onnx.save(graph, path)
            onnx.checker.check_model(graph)
            files.append(
                {
                    "name": name,
                    "file": path.name,
                    "bytes": path.stat().st_size,
                    "sha256": hashlib.file_digest(
                        path.open("rb"), "sha256"
                    ).hexdigest(),
                }
            )
            attribution(
                path,
                f"FilmCraft native {name} model",
                "Meta / HUST",
                "https://github.com/facebookresearch/EdgeTAM"
                if name != "matte"
                else "https://huggingface.co/hustvl/vitmatte-small-distinctions-646",
                "Apache-2.0",
                "Offline ONNX conversion of pinned original checkpoint",
            )
        manifest = {
            "version": FORMAT_VERSION,
            "edge_revision": EDGE_REV,
            "edge_sha256": EDGE_SHA,
            "matte_revision": MATTE_REV,
            "matte_sha256": MATTE_SHA,
            "opset": OPSET,
            "files": files,
        }
        (destination / "manifest.json").write_text(json.dumps(manifest, indent=2))
    print(f"Prepared native models at {destination}", flush=True)


def verify_export(destination):
    expected = json.loads(Path(__file__).with_name("native-models.json").read_text())
    path = destination / "manifest.json"
    if path.stat().st_size > 16 * 1024 or json.loads(path.read_text()) != expected:
        raise ValueError("Native export differs from the pinned model package")
    for entry in expected["files"]:
        path = destination / entry["file"]
        with path.open("rb") as file:
            checksum = hashlib.file_digest(file, "sha256").hexdigest()
        if path.stat().st_size != entry["bytes"] or checksum != entry["sha256"]:
            raise ValueError(f"Native {entry['name']} graph checksum differs")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=DATA / "models" / "native")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if not args.check:
        export(args.output)
    verify_export(args.output)
