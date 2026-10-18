# ADR 0002: resident Windows decode frames through D3D11 / wgpu DX12

- **Status:** proposed; extends the unsafe boundary in [ADR 0001](0001-platform-ffi.md), subject to owner approval
- **Origin:** [PR #152](https://github.com/storytold/filmcraft/pull/152), adapted by [PR #462](https://github.com/storytold/filmcraft/pull/462)
- **Issue:** [#30](https://github.com/storytold/filmcraft/issues/30)

## Context

Media Foundation decodes NV12/P010 into D3D11 surfaces. CPU-only output requires staging readback and planar conversion; GPU preview then uploads those pixels again. The proposed path removes that CPU round trip when the compositor can share the decoder adapter's memory. It still performs a GPU-to-GPU copy into a shareable texture; “zero-copy” here means no CPU pixel transfer on the successful import path.

## Decision

1. Keep the frame contract independent of OS/wgpu types. `PixelData::Gpu(GpuPixels)` owns `Arc<dyn GpuSurface>`; `VideoFrame::cpu` preserves metadata and shares one cached download result across clones. Conversions and transforms return `Result`; preview, render/effects, export and analysis propagate terminal failures instead of substituting gray pixels. This shared API is a prerequisite PR; it introduces no Windows interop or backend policy.
2. `media_foundation::Decoder::share` copies the eligible cropped picture into an independent shareable D3D11 NV12/P010 texture. The decoder waits for GPU copy completion before exposing it. Even, nonzero 4:2:0 geometry and supported size are required; ineligible geometry retains the existing CPU readback path.
3. `SharedSurface` records its creating adapter's LUID; `media_foundation::interop::import` checks that identity against the target DX12 device on each import before opening the NT handle. It wraps luma/chroma as wgpu textures: R8Unorm/Rg8Unorm for NV12, R16Unorm/Rg16Unorm for P010. The compositor importer retains the planes and surface owner while cached. Imported and uploaded sources use the same compositor, effects, transitions and YUV range math; there is no second renderer.
4. Select DX12 only when DXC libraries are available and compositor construction succeeds. Respect explicit backend overrides; `FILMCRAFT_NO_ZERO_COPY=1` retains normal renderer selection. `FILMCRAFT_DXC_DIR` or the executable directory supplies `dxcompiler.dll`/`dxil.dll`; bundling them remains a packaging decision. Enable resident decode only after same-adapter DX12 compatibility is established.

## Unsafe boundary and ownership

Only `crates/platform/src/media_foundation/interop.rs` extends the FFI allowlist. It creates shared D3D11 resources/handles, reads adapter LUID, opens D3D12 resources and wraps them through wgpu HAL. Existing `gpu`/`mft` modules retain their OS media roles. Every unsafe block requires its own safety argument; public APIs remain safe and return errors.

`SharedSurface` owns its D3D11 texture and closes its NT handle once on drop. Imported D3D12 resources hold COM references; the importer also retains `Arc<SharedSurface>`. The D3D11 device is multithread protected; completed copies precede cross-device reads. wgpu owns the HAL wrappers and tracks compositor submissions. Resource-state, format/plane, thread-safety and queued-use assumptions require review against the actual `interop` implementation; this ADR does not replace that review.

## Failure contract

- Unsupported renderer/adapter/geometry keeps CPU decode output. A share failure disables sharing for that decoder stream and attempts the existing readback path.
- Import failure logs the cause and tries the picture's cached CPU materialization/upload. Materialization failure remains an error; it must not enter successful frame caches or encoder input.
- P010 import requires `TEXTURE_FORMAT_16BIT_NORM` on the renderer device; missing support returns an import error before HAL wrapping. NV12 remains eligible on that device.
- Disabling the compositor stops new GPU pictures. Existing pictures retain their owners and may still require a download; failure must be reported without losing project edits. Hardware-to-software decoder replay remains the hybrid decoder's separate responsibility.

Actual device-loss recovery, including subsequent playback/export progress, is not yet verified. The original proposal's “never lose a picture” wording is a goal, not an established guarantee. Shared-API injected download failures do not prove Windows device-loss recovery.

## Cost and acceptance

GPU-resident pictures retain texture memory and an NT handle; CPU consumers additionally pay a download and retain its cached result. Imported-source eviction and GOP/frame-plan budgets limit parts of this ownership graph, but do not prove a sustained total-memory bound. Surface pooling is excluded until cross-device queued use can be tracked safely.

Before merge: green `cargo xtask ci`; same-picture NV12/P010 import/readback and compositor comparisons with responsive changed-picture controls; unsupported-backend/import-failure controls; actual device-loss recovery; sustained playback/seek/transition/cache-eviction memory and handle bounds. Performance results must identify hardware, build, workload and measured bands. No WebCodecs integration or Web performance claim is included.
