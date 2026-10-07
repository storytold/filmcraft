# ADR 0002: zero-copy hardware decode on Windows (D3D11 surface to wgpu DX12)

- **Status:** proposed (needs the project owner's approval, like [ADR 0001](0001-platform-ffi.md))
- **Issue:** #30 (hardware acceleration), follow-up of the Windows Media Foundation decoder

## Context

The Windows hardware decoder (`crates/platform/src/media_foundation/`) decodes on the GPU but copies
every picture back to system memory (staging texture, then planar `Yuv8` / `Yuv16`): about 10 ms of
CPU per 2160p picture, and the compositor then uploads the same pixels to the GPU again.

## Decision

1. **A picture can live in GPU memory.** `filmcraft_frame::PixelData::Gpu(GpuPixels)` holds an
   `Arc<dyn GpuSurface>` (a trait with no OS or wgpu types, so the frame crate stays portable and
   wasm-clean). Every CPU consumer reads it through `VideoFrame::cpu()`, which downloads the picture
   once and caches it with the surface; the CPU operations on `VideoFrame` (`to_linear_f32…`,
   `to_rgba8`, `luma8`, `rotated`) do this themselves, so code that does not know about GPU pictures
   keeps working. `byte_size` counts the GPU memory, so the GOP caches' budgets still hold.
2. **The decoder shares, the compositor opens.** The decoder copies the cropped picture GPU to GPU
   into an NV12 / P010 Direct3D 11 texture created shareable (NT handle) and waits for the copy
   (`ID3D11Query` event: the GPU only, no pixel moves). `filmcraft_gpu::set_surface_importer`
   (registered by `media_foundation::enable_zero_copy`) lets the compositor open it on its own
   device: `OpenSharedHandle` gives a Direct3D 12 resource, and `wgpu::hal::dx12` wraps its two planes
   as single-plane textures (`R8Unorm` + `Rg8Unorm`, or `R16Unorm` + `Rg16Unorm`), which a new
   compositor texture kind (3: luma plus interleaved chroma) samples like the planes it uploads now.
3. **Containment.** The `unsafe` is in `media_foundation::interop` (the shared texture, the hal
   wrap), next to the existing FFI modules, under ADR 0001's rules (`// SAFETY:` on every block, safe
   `Result` API, no raw pointers or COM types in public signatures). This extends the crate's one job,
   OS media FFI, to handing its output to the renderer; it still does nothing else.
4. **Opt-in by the renderer.** Zero-copy needs a wgpu **DX12** device on the decoder's adapter
   (LUID check). wgpu's default backend on Windows is Vulkan, and DX12's default shader compiler
   (FXC) cannot compile the compositor's effects shader (`fx.wgsl`: "compilation aborted
   unexpectedly"), so the desktop app uses DX12 only when the DirectX Shader Compiler
   (`dxcompiler.dll` + `dxil.dll`, from <https://github.com/microsoft/DirectXShaderCompiler/releases>,
   LLVM-exception Apache 2.0) is next to the executable or in `FILMCRAFT_DXC_DIR`, and only after the
   whole compositor has been built on that device. Otherwise nothing changes: Vulkan, CPU readback.
   `FILMCRAFT_NO_ZERO_COPY=1` keeps the default renderer. Shipping DXC in the installer is a packaging
   decision this ADR leaves open.

## The fallback guarantee

- A device that is not DX12 on the decoder's adapter, a failed import (logged, the picture is
  downloaded and uploaded as before), a failed copy (the decoder switches to the readback path for
  the stream), or a compositor dropped after a GPU error (`set_gpu_frames(false)`) never lose a
  picture; the hybrid decoder's software fallback is unchanged.
- Pictures are interchangeable: the CPU download is bit-exact with the software decoder, and the
  compositor's output from the shared surface matches the uploaded picture to within one 8-bit level
  (tests/zero_copy.rs; H.264 High, HEVC Main, HEVC Main 10).

## Consequences

- 2160p playback CPU per frame drops again (see [performance.md](../performance.md)), and the
  decoder worker no longer copies or converts pictures.
- GPU pictures cost video memory while cached (counted in the cache budget) and one NT handle each;
  surfaces are not pooled yet (a pool needs the Direct3D 12 side's use to be tracked).
- Export and other CPU consumers pay one download per picture, as before the change, plus the shared
  copy; an export-time switch (`set_gpu_frames(false)` around exports) is a follow-up.
- The Linux (VA-API / Vulkan Video) and macOS (IOSurface / Metal) backends can implement the same
  `GpuSurface` and importer hook.
