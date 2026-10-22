# Source PR CI evidence

`ci.yml` runs the existing `cargo xtask ci` gate on opened, synchronized,
reopened and ready-for-review PRs, without source/path or draft filters, plus main
pushes and manual dispatch. The existing platform packaging triggers are unchanged.
The read-only hosted Linux job tests the PR merge result and records both head
and checkout identities. No PR-target execution or signing/deployment credentials
are used. The 150-minute cold-build limit is provisional for this new job layout.

The CI environment disables release-profile LTO and uses 16 codegen units for
`cargo xtask ci`. Tests retain release optimization and the existing gates;
release packaging keeps the manifest's ThinLTO/single-unit configuration. The first
hosted runs exhausted 150 minutes compiling test targets before any tests ran.

The runner installs Mesa Lavapipe and probes its Vulkan adapter. A failed probe
does not prevent CPU gates from running, but fails the final coverage report.
The Vulkan ICD selects Lavapipe without overriding `WGPU_BACKEND` for every test;
startup-policy tests must still observe the upstream default backend set.
`RUST_TEST_NOCAPTURE=1` exposes the existing GPU tests' adapter/parity/skip messages.
The reporter requires a successful gate, executed Rust tests and positive GPU
evidence; runtime no-adapter skips fail acceptance. Intentionally ignored platform
cases are counted/reported separately. Complete logs and source identity are saved
as artifacts. Software Vulkan coverage does not qualify physical GPU drivers,
other operating systems or tests deliberately ignored on Linux.

Run the report regression tests with
`python3 -m unittest discover -s .github/scripts -p 'test_*.py' -v`.
Trigger simulation and fixture tests do not prove GitHub events or runner images
work: after approved publication/landing, inspect actual PR run URLs and results,
including a ready-for-review event, before requiring the new check for merges.
