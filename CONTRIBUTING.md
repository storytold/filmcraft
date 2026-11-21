# Contributing to FilmCraft

Thanks for helping. Before you start:

1. Read **[AGENTS.md](AGENTS.md)**: the shared rules and the facts about this repository.
2. **Never crash.** A crash loses someone's work, so this rule outranks feature work: no `unwrap()`,
   `expect()`, `panic!`, `unreachable!`, `todo!` or `unimplemented!` in production code, and no `unsafe` outside `crates/platform`;
   return `Result` and treat every input as hostile. See rule 13 of [AGENTS.md](AGENTS.md).
3. Follow **[docs/contributing.md](docs/contributing.md)**: setup per OS, building and running,
   the checks, commit conventions, and how to add commands, effects,
   transitions, codecs, panels and assets.

More reading: [docs/architecture.md](docs/architecture.md), [docs/testing.md](docs/testing.md),
[docs/agents.md](docs/agents.md), [ROADMAP.md](ROADMAP.md).

Contributions are licensed under MIT OR Apache-2.0, the project licence.
