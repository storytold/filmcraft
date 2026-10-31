---
title: Install FilmCraft
description: Download FilmCraft for Windows 11, macOS or Linux, from getartcraft.com, GitHub or ArtCraft Launcher, and pick the right file.
order: 10
verified: 0.7.2
---

The easiest way is to download FilmCraft from the [FilmCraft page on getartcraft.com](https://getartcraft.com/apps/filmcraft). If you want a specific file, take it from the [latest release](https://github.com/storytold/filmcraft/releases/latest) on GitHub. In the file names, `<version>` stands for the version number, for example `0.7.2`.

You can also install it with ArtCraft Launcher, which lists FilmCraft with the other Craft apps. See [ArtCraft Launcher](https://getartcraft.com/docs/craft-launcher).

## Windows 11

1. Open the [latest release](https://github.com/storytold/filmcraft/releases/latest).
2. Download `filmcraft-<version>-windows-x64.msi`. It fits most PCs (64-bit Intel and AMD).
3. Run the file and follow the installer.

| Your PC                            | Installer                               | Portable                                         |
|------------------------------------|-----------------------------------------|--------------------------------------------------|
| 64-bit Intel or AMD                | `filmcraft-<version>-windows-x64.msi`   | `filmcraft-<version>-windows-x64-portable.zip`   |
| Arm (Snapdragon and other Arm PCs) | `filmcraft-<version>-windows-arm64.msi` | `filmcraft-<version>-windows-arm64-portable.zip` |
| 32-bit Windows                     | `filmcraft-<version>-windows-x86.msi`   | `filmcraft-<version>-windows-x86-portable.zip`   |

The installers and executables are code-signed. If FilmCraft does not open after you install it, see [FilmCraft does not open on Windows](../troubleshooting/filmcraft-does-not-open-on-windows.md).

## macOS

1. Download `filmcraft-<version>-macos-universal.dmg`. It works on Apple silicon and Intel Macs, and it is signed and notarized.
2. Open it and install FilmCraft.

The release also has `filmcraft-cli-<version>-macos-universal.zip`, the command-line tool.

## Linux

Pick one file from the release. Replace `x86_64` with `aarch64` on Arm.

| Format                 | File                                        | Notes                                                           |
|------------------------|---------------------------------------------|-----------------------------------------------------------------|
| AppImage               | `filmcraft-<version>-linux-x86_64.AppImage` | Runs anywhere. The `.zsync` files let AppImageUpdate update it. |
| Flatpak                | `filmcraft-<version>-linux-x86_64.flatpak`  | Sandboxed. Install with `flatpak install --user <file>`.        |
| Debian and Ubuntu      | `filmcraft-<version>-linux-x86_64.deb`      |                                                                 |
| Fedora, RHEL, openSUSE | `filmcraft-<version>-linux-x86_64.rpm`      |                                                                 |
| Tarball                | `filmcraft-<version>-linux-x86_64.tar.gz`   | Unpack it and run `./install.sh`.                               |

The tarball installer puts FilmCraft, its command-line tool and the desktop entry in `~/.local`, with no administrator rights. For all users, run `sudo ./install.sh --prefix /usr/local`. Running the installer again updates the installation.

RISC-V (riscv64) has a `.deb`, an `.rpm` and a tarball, named with `riscv64`, but no AppImage or Flatpak. It needs glibc 2.39 or newer, such as Ubuntu 24.04 or Debian 13.

## FreeBSD

Download `filmcraft-<version>-freebsd-x86_64.tar.gz`.

## In a browser

Every release includes `filmcraft-web-<version>.zip`, a static site that runs in a modern browser. Host it on any static web server.

## Check your download

`SHA256SUMS.txt` on the release page lists a checksum for every file.
