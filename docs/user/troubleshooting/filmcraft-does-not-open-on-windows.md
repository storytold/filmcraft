---
title: FilmCraft does not open on Windows
description: Fix FilmCraft flashing in Task Manager or quitting right after launch on Windows 11, caused by a graphics driver crash, by letting it switch to a safe graphics mode or choosing one yourself.
order: 10
verified: 0.7.2
---

Start FilmCraft again a few times, waiting 15 seconds between tries. After two failed starts in a row it switches to a safe graphics mode by itself. Updating your graphics driver may also fix it.

## Symptoms

- You double-click FilmCraft and nothing appears, or it shows up in Task Manager for a moment and quits.
- Windows Event Viewer (Windows Logs ▸ Application) shows an access violation (`0xc0000005`) in a graphics driver file. On Intel graphics this was `igvk64.dll`, in the Vulkan driver. On some AMD systems it was `atio6axx.dll`.

## Fix

1. Update your graphics driver from your GPU maker. This may not be enough: the Intel laptop in issue 1037 crashed the same way on the latest Intel driver.
2. Start FilmCraft again. Wait about 15 seconds between tries: a start counts as failed only when FilmCraft quit within 15 seconds of launch without showing its window.
3. After two failed starts in a row, the next start uses DirectX 12 only, without Vulkan. If that fails twice too, FilmCraft moves on to software rendering. It keeps the mode that works for later starts.
4. In a safe mode, FilmCraft shows a notice at the bottom right: **Safe graphics mode: DirectX 12 only** or **Safe graphics mode: software rendering**. Some effects may then run on the CPU instead of the GPU, and with software rendering the preview is drawn on the CPU.

To go back to the normal graphics mode, for example after a driver update, click **Use Default Graphics** in the notice. FilmCraft uses the default mode from the next start. **Close** hides the notice for this session.

## Choose the graphics mode yourself

You can pick the mode when you start FilmCraft from PowerShell. Go to the folder that holds `filmcraft.exe` and run one of these:

    & .\filmcraft.exe --graphics dx12
    & .\filmcraft.exe --graphics software
    & .\filmcraft.exe --graphics default

FilmCraft keeps the mode you pick for later starts. `--graphics default` undoes an automatic switch.

The older workaround still works: set `WGPU_BACKEND` to `dx12`, `vulkan` or `gl` before you start FilmCraft. While it is set, FilmCraft does not switch modes by itself. On Intel graphics, avoid `vulkan`. On AMD graphics, avoid `gl`: on some AMD drivers an OpenGL start can bring the crash back. To undo it in the same PowerShell window, run `Remove-Item Env:WGPU_BACKEND`.

## Why this happens

By default FilmCraft starts Vulkan and DirectX 12 on Windows, not OpenGL. Some graphics drivers crash inside the driver while the window is being created, before FilmCraft can handle an error. Intel's Vulkan driver and AMD's OpenGL driver have both done this. FilmCraft notices such crashes on the next start and moves to a mode that avoids the crashing driver.

## Still stuck?

[Report a problem](send-a-bug-report.md) and include your `filmcraft.log`. Or ask in [Discord](https://discord.gg/artcraft). Related reports are [issue 1037](https://github.com/storytold/filmcraft/issues/1037), a driver crash on Intel graphics, [issue 1134](https://github.com/storytold/filmcraft/issues/1134), FilmCraft not opening on Windows 11, and [issue 820](https://github.com/storytold/filmcraft/issues/820), the automatic switch to a safe graphics mode.
