---
title: Report a problem
description: What to include when you report a FilmCraft bug, and where the log files are.
order: 20
verified: 0.7.2
---

Open an issue in the [issue tracker](https://github.com/storytold/filmcraft/issues) or tell the team in [Discord](https://discord.gg/artcraft). Bug reports with real footage are the most useful thing you can send.

## What to include

- What happens, and what you expected.
- The steps you took right before it happened.
- Your operating system and version, your GPU, the FilmCraft version, and how you installed it (installer, portable zip or built from source).
- For a media problem, the file's codec and container.
- The log files `filmcraft.log` and any `crash-*.log` files.

## Find the log files

If FilmCraft opens, choose **Help ▸ Reveal Log Files…**. It opens the folder. Otherwise look here:

| System  | Folder                                         |
|---------|------------------------------------------------|
| Windows | `%APPDATA%\FilmCraft\Logs`                     |
| macOS   | `~/Library/Application Support/FilmCraft/Logs` |
| Linux   | `~/.local/share/filmcraft/Logs`                |

On Windows, paste `%APPDATA%\FilmCraft\Logs` into the Explorer address bar. Each start keeps the previous log as `filmcraft.1.log`, and the one before as `filmcraft.2.log`, so the log of a run that crashed survives the next launch. A portable build keeps its data in a `data` folder next to the program, and the `FILMCRAFT_DATA_DIR` setting moves it anywhere.

## From inside the app

The **Help** menu also has **Report an Issue…** and **System Compatibility Report…**. Add the report text to your issue.
