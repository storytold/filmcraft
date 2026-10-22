# filmcraft-format

The project JSON envelope, schema migrations and compatibility checks. The
crate reads older supported schemas through append-only migrations and rejects
newer schema versions before deserializing project data.

Schema 16 permits the explicit Apple Log variant in cached video colour
metadata. The v15 migration preserves existing data and user Interpret Footage
overrides; no old colour metadata is guessed or rewritten. The version bump
prevents older readers from accepting the new enum without understanding it.

Run `cargo test --release -p filmcraft-format` and `cargo xtask ci`. Engine
colour tests also save/reload detected Apple Log and migrate a v15 project
with an explicit Rec.709 override.
