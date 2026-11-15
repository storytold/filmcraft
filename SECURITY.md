# Security policy

FilmCraft processes untrusted media, projects, interchange files, fonts and presets,
and provides CLI, control-channel and MCP automation. Security reports about those
inputs, automation boundaries, dependencies or release artifacts are welcome.

## Reporting a vulnerability

GitHub private vulnerability reporting is not currently enabled for this repository,
and no dedicated private security address is documented. Contact the maintainers
through the [project's existing community channels](https://discord.gg/artcraft)
with a minimal, non-exploitable description and ask for a private transfer method.
Do not publish a working exploit, malicious attachment, credential, private project,
or sensitive crash dump in a public issue or chat. Wait for a private channel before
sharing those details; if none is established, withhold sensitive material.

A useful report includes the FilmCraft version or commit, operating system and
architecture, affected component, impact, reproduction steps, and any workaround.
For media or project parsing problems, initially describe the file format, size,
cryptographic hash and whether the sample is synthetic or privately owned. Remove
personal paths, credentials and unrelated project content from logs. Coordinate
sample transfer with a maintainer instead of uploading a malicious file publicly.

Please allow maintainers time to investigate and coordinate a fix before disclosing
exploitable details. This policy does not promise a response deadline or a particular
supported-version window. Include the exact affected version and, when safe to do
so, whether the problem also occurs on the latest release.

## Regression samples

A regression fixture must be minimized, legally redistributable, free of private
content, and documented with the expected safe failure. Follow [AGENTS.md](AGENTS.md)
for clean-room implementation, asset attribution and the no-crash rules. Large media
fixtures belong in the project's approved corpus workflow rather than ordinary PRs.

Ordinary feature requests, editing problems and usage questions should use the
normal issue forms or community help channel.
