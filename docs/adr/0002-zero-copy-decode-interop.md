# ADR 0002: resident Windows decode frames through D3D11 / wgpu DX12

- Status: proposed; extends the FFI boundary in [ADR 0001](0001-platform-ffi.md), subject to owner approval
- Origin: [PR #152](https://github.com/storytold/filmcraft/pull/152), adapted by [PR #462](https://github.com/storytold/filmcraft/pull/462) and [PR #1021](https://github.com/storytold/filmcraft/pull/1021)
- Issue: [#30](https://github.com/storytold/filmcraft/issues/30)

## Decision

Media Foundation returns NV12/P010 D3D11 textures. Copy eligible cropped pictures into independent shareable textures, wait for copy completion, then open their NT handles on a compatible wgpu DX12 device. This removes the CPU readback/upload round trip; a GPU-to-GPU copy remains. Unsupported geometry or sharing retains CPU output.

The shared frame API owns `Arc<dyn GpuSurface>` through `GpuPixels`; fallible CPU materialization is cached across clones. OS/wgpu types stay outside that contract. CPU render, analysis and export propagate materialization errors; failed frames must not become successful cache entries or substitute pixels.

`media_foundation::interop::import` checks the surface's adapter LUID against the actual target device on every import. NV12 uses R8Unorm/Rg8Unorm planes; P010 uses R16Unorm/Rg16Unorm and requires `TEXTURE_FORMAT_16BIT_NORM`. Imported sources use the existing compositor, effects and YUV math. Tagged CPU allocation / GPU surface identities prevent cache aliasing; the existing source-cache bound applies to both.

Only normal startup may select DX12, after both DXC libraries are found and compositor construction succeeds. Respect explicit backend overrides and recovery modes. `FILMCRAFT_NO_ZERO_COPY=1` retains normal renderer selection; `FILMCRAFT_DXC_DIR` or the executable directory supplies dxcompiler.dll/dxil.dll. Packaging those libraries remains a separate decision. Enable resident decode only after the active compositor's device passes the same-adapter check.

## Ownership and failures

`media_foundation::interop` is the added unsafe FFI module: D3D11 shared resources/handles, adapter identity, D3D12 resource opening and wgpu HAL wrapping. Public APIs remain safe and return errors; each unsafe block states its invariants. Decoder policy and `MfSurface` stay safe Rust.

`SharedSurface` owns the D3D11 texture and closes its NT handle once. Imported D3D12 resources retain COM references; cached planes also retain the surface owner. The D3D11 device is multithread protected and completed copies precede cross-device reads. Resource state, plane format and queued-use ownership remain part of the interop safety contract.

A share failure disables sharing for that decoder and attempts CPU readback. A copy-completion failure returns a decoder error without publishing the batch; the hybrid decoder replays buffered samples in software or reports terminal failure. Import failure tries cached CPU materialization/upload; materialization failure remains an error. Disabling the compositor stops new resident output; existing frames retain their owners and fallible download path.

## Cost and acceptance

Resident pictures retain GPU memory and handles; CPU consumers additionally retain downloaded planes. Bounded compositor caching and frame/GOP budgets constrain parts of this graph, not total memory. Pooling surfaces requires explicit cross-device queued-use tracking and is outside this decision.

`tests/zero_copy.rs` remains an independent test binary because decoder policy and surface counters are process-wide. It covers decoded-plane/compositor parity, Vulkan materialization fallback, cache eviction/release and controlled renderer replacement. Its CPU reference explicitly downloads planes, so it does not measure a download-free workload. UI/export device-destruction tests separately check changed output through CPU fallback. These are unit/selftest and isolated runtime gates, not physical driver reset or full-window recovery evidence.

Acceptance also requires integrated quality gates, same-adapter hardware execution without required-case skips, and sustained playback/seek/transition runs with responsive memory/handle/surface counters and final-release checks. Physical decoder-device loss, driver reset and preservation of project edits need separate lifecycle validation. Results must name the tested revision, hardware and workload; finite runs do not establish an indefinite memory bound. This Windows contract provides no WebCodecs or Web performance evidence.
