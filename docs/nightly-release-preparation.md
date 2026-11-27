# Windows, Linux and macOS release-ref nightly: source preparation

This is the FilmCraft port of the ArtCraft Nightly kit (PhotoCraft #16335, #16559, #16459), landed on
top of the Linux packaging work (`storytold/filmcraft#5225`). The producer and signer are enabled for
supervised manual dispatch only: there is no schedule, `.github/nightly-signing-policy.json` is
installed with the five release managers as its only manual actors, and no run has signed or
published anything yet.

`packaging/nightly-tests.py` and `docs/nightly-signing-policy.example.json` are the source contract;
`packaging/nightly-release-tests.py` enforces it (one shared pin, one producer, one signer, exact
platform list, trust anchors, no cache/secrets in the callers' closure).


**Manually enabled, not qualified.** A run needs a release manager's dispatch: both callers keep
their exact context guards, the policy grants no automatic signer, and there is no schedule or
platform secret grant. Nothing has produced a qualified nightly, so the pipeline is still
unproven end to end. The unsigned reusable caller's maximum OIDC/attestation
permissions are for its isolated attester; its compiler narrows to contents and Actions read
only, with no OIDC, attestation write, release environment or platform signing secrets.
Shared tooling is pinned to `be00e5ccd18b68e950c8a08532f2230e7c3d0cba`, the craft-common-actions
main commit that merged [#371](https://github.com/storytold/craft-common-actions/pull/371) (the
candidate is admitted on the built commit's own Sonar analysis, not main's latest) after
[#370](https://github.com/storytold/craft-common-actions/pull/370) (the Windows and macOS nightly
registry rows for the 14 apps) and
[#369](https://github.com/storytold/craft-common-actions/pull/369) (shared helper checkouts kept out
of the app's clean-source check, and macOS on `macos-15`), following
[#364](https://github.com/storytold/craft-common-actions/pull/364) (the signer may run on the
policy's signer branch) and [#363](https://github.com/storytold/craft-common-actions/pull/363) (the
multi-app pipeline, which makes the signer's `app-name` input required and admits any reviewed
registry app), with #342 (`a4f6fd51`, policy v3 and the `platforms` input) and #339 (`605d5876`)
earlier. Both callers name it in `uses` and `tooling-revision`, and the installed policy names it as
both tooling revisions; the earlier pin, `077d4657` (#330), is Windows-only. A pin is reviewed
source availability, not provider, queue, native signing or crypto qualification.
**Signer branch.** The signer runs on `refs/heads/nightly-signer`, not on the stable `release`
branch, so nightly signer files never need a stable release sync. The policy's `release_branch`
names it, the signer's guard requires it and its caller passes `app-name`. The branch only
fast-forwards to reviewed main commits, through the nightly-signer settings script's `advance` (no
queued signer run, a trust-closure scan, then the full diff of `.github/` and `packaging/`), and the
existing `release` environment trusts it alongside `release`. Its trust closure is all of `.github/`
and `packaging/`, including `packaging/env.sh`, which `import-cert.sh` sources with the Apple keys.
On that branch only the pinned `nightly-sign.yml` caller may reach an environment: no app workflow
binds one, nothing else calls `nightly-release.yml`, and the only caller of the shared stable
`release.yml` is `release.yml`, whose callee must carry the stable-only environment guard. The
contract tests check the in-repo part on every main PR; the settings scan also resolves the callee.
Stable releases still come only from `refs/heads/release`. History does not carry over a switch
between `release` and `nightly-signer`, so the switch happens before the first publication.

Existing stable workflows are unchanged.

**Platform rows.** The producer caller lists
`platforms: '["windows-x86_64","linux-x86_64","macos-aarch64"]'` on its single job: one producer run
builds every listed platform for one all-or-nothing release (one failed or
skipped platform stops that night's release, Windows included), and the release-ref signer admits
platforms only from the protected policy, so there is no separate Linux or macOS caller job and
the signer caller has no platform input. The shared Linux leg runs the app's
`packaging/linux/nightly.sh --arch x86_64` (GUI-only `filmcraft-<version>-linux-x86_64-nightly.tar.gz`)
and signs with the stable OpenPGP key; the macOS leg runs `packaging/macos/nightly.sh --arch aarch64`
unsigned on main, then `packaging/macos/nightly-sign.sh` (stable `import-cert.sh`, Developer ID,
hardened runtime, notarization and stapling) and `nightly-verify.sh` from the protected `nightly-signer`
checkout. Those scripts are owned by FilmCraft's Linux and macOS packaging authors, not by these
callers. Neither leg has run on hosted runners: launch, profile isolation beside stable and the
Launcher install still need Linux and macOS desktop QA.
This is not a hosted no-key test, a native signing result or a working nightly feed.

## Ownership and interface

- `.github/workflows/nightly.yml`: manual-dispatch producer on default main, named
  `FilmCraft unsigned nightly`, restricted to the five release managers; no signing secrets may be
  passed or inherited.
- `.github/workflows/nightly-dispatch.yml`: completion transport. It accepts
  only a successful same-repository/main first producer attempt, then sends only
  `producer_run_id` and `producer_run_attempt` to the fixed `nightly-sign.yml` on
  `refs/heads/nightly-signer`.
  No checkout, artifact parsing, application code or release environment is used here.
  Its filters are not authorization: a writer can edit main transport.
- `.github/nightly-linux-signing-key.asc`: the stable Linux release public key, byte for byte the
  `release-signing-key.asc` published with the newest stable FilmCraft release (one ed25519 primary key,
  `2B76741B414AAD6CD33E631213A005C0B9D7C3ED`, with one signing subkey). The shared Linux verifier
  reads it from the protected `nightly-signer` checkout, never from a release asset. Public material only.
- `.github/workflows/nightly-sign.yml`: manual-dispatch signer on `nightly-signer`, which only ever
  fast-forwards to reviewed main commits. It rejects wrong repository/event/actual branch,
  including same-name tags. Its constant whole-run concurrency is distinct from stable,
  cancellation is off and `queue: max` must be hosted-qualified. No credentials can be reached
  by this preparation.
- Shared reusable entrypoints are `nightly-unsigned.yml` and `nightly-release.yml` in
  `storytold/craft-common-actions`. Both must resolve to a **real reviewed public immutable
  commit**; the defining workflow SHA and tooling input must match. The release callee's
  request inputs are consistently `producer_run_id` and `producer_run_attempt`, while
  `tooling-revision` keeps its existing hyphen spelling. Confirm the final landed interface
  before any future pin update or activation. Unsigned inputs are `tooling-revision`, `app-name`,
  `manual-actors` (default `[]`), `craft-fonts-ref` and `platforms`; FilmCraft's title is reviewed source,
  not an `app-title` input. The reusable caller grants maximum permissions only; each
  callee job must narrow them independently.
  Shared code owns attestation validation, candidate quality, archive bounds, native signing,
  role-separated receipts and publication. App code must not duplicate those parsers.
- The existing `packaging/windows/sign.ps1 -Required` is the app-owned Windows adapter.
  Invoke it only from the immutable **release execution SHA** checkout, never the producer's
  main source or extracted archive. Shared Windows tooling must own the flat GUI-only ZIP,
  PE/inventory checks and post-sign verification; do not execute `nightly.ps1`, `package.ps1`
  or the payload beside keys. No new wrapper is needed. `sign.ps1` is unchanged here.

## Missing real configuration (all activation blockers)

`.github/nightly-signing-policy.json` is deliberately absent: there is no authorizing root,
empty debt set or invented workflow ID, and installing it is a separate activation step. The adjacent
[`nightly-signing-policy.example.json`](nightly-signing-policy.example.json) is intentionally
invalid (`null` unresolved values), not installable configuration. Its `platforms` rows do carry
the reviewed public values: each fixed `nightly-unsigned-<platform>` artifact kind and stable's
identities, read from the newest stable FilmCraft release (Windows Authenticode leaf subject in Windows form,
the Linux release key's primary fingerprint, and the Developer ID team `DJ6XS33FX8` from the
macOS CLI's code signature). Install only independently
read, reviewed real values under explicit schema `craft-nightly-release-policy/v3` (v2 top-level fields
without `platform`, `unsigned_artifact_name` and `windows_publisher_subject`, plus a `platforms` map):

| Field | Required source of authority |
|---|---|
| `repository`, `repository_id` | Exact FilmCraft repository and authenticated numeric ID |
| `producer_workflow_id`, `producer_workflow_path` | Real default-branch registration ID and `.github/workflows/nightly.yml` |
| `producer_tooling_revision`, `producer_workflow_ref` | Approved shared unsigned producer/attester commit and exact reusable path at that commit |
| `signer_workflow_path`, `signer_tooling_revision` | `.github/workflows/nightly-sign.yml` and reviewed public shared signer commit |
| `release_branch`, `platforms` keys | `release`; exactly the producer's `platforms` list and the targets admitted in the shared registry (`windows-x86_64`, `linux-x86_64`, `macos-aarch64`) |
| `manual_actors` | Empty by default; reviewed manual producer/signer and rerun authority only |
| `automatic_signer_actors` | Empty by default; separate reviewed automatic-only authority for signer attempt 1 after authenticating a scheduled producer attempt 1; no manual producer or rerun grant |
| `max_age_seconds`, `max_artifact_bytes`, `platforms.<platform>.unsigned_artifact_name` | Reviewed bounded age/size and each platform's fixed unsigned artifact kind/prefix; actual upload/discovery name is that prefix + `-` + authenticated producer run ID + `-` + authenticated producer attempt, never a caller pattern or substitution |
| `candidate_policy` | Full reviewed `craft-nightly-candidate-policy/v3` with real required workflow/job/check/app IDs, Sonar organization/project/issue types/conditions and exact approved debt fingerprints; provider configuration is trusted as resolved, not a pinned scan inventory |
| `platforms.linux-x86_64.trust.fingerprint` | Full primary fingerprint of the stable Linux release key, independently reviewed; the release checkout must also hold exactly that one public key at `.github/nightly-linux-signing-key.asc`, and the existing `LINUX_SIGNING_KEY` must be in the `release` environment only |
| `platforms.macos-aarch64.trust.apple_team_id` | Stable's ten-character Developer ID team `DJ6XS33FX8`, read from the stable macOS code signature, whose leaf is `CN=Developer ID Application: Learning Machines LLC (DJ6XS33FX8), OU=DJ6XS33FX8, O=Learning Machines LLC`. The Windows certificate names a different entity, `Learning Machines Inc`; only the team ID `DJ6XS33FX8` is enforced on macOS. Never a repository variable. The six stable `APPLE_*`/`KEYCHAIN_PASSWORD` names must be in the `release` environment only |
| `platforms.windows-x86_64.trust.publisher_subject` | Exact independently reviewed protected policy string used by both signer and fresh credentialless Windows publisher; nonempty, at most 2048 characters, no CR/LF/NUL; verified native subject must match exactly, never an artifact/caller input |

Live admission requires explicit policy v3 once the shared parser lands it; v2 is single-platform
Windows only and is not silently migrated. Historical v1 retains its exact old fields and
broad manual-list semantics, and is not silently migrated or accepted for live admission. The
v2 actor lists are separate, bounded and case-normalized; overlaps refuse. An automatic-only
actor may authorize **only signer attempt 1**, after the validator authenticates a **scheduled
producer attempt 1**. That list grants no manual producer, producer rerun or signer rerun
authority. Those requests require the independently reviewed `manual_actors` list.

Before future automatic dispatch is activated, its identity needs explicit review for
`automatic_signer_actors`; no bot is implicitly trusted or installed here. Never union that
list into `manual_actors` or the unsigned caller's `manual-actors` input. Both example lists
and the caller input remain empty; this preparation does not populate or authorize policy.

### Post-publication recovery limitation

The pinned shared revision now prevents a narrowly scoped failure, not general recovery:
**only after the publisher returns through successful final verification** may an ownership
receipt I/O failure be reported without failing that already-verified publication. A public
PATCH callback alone is insufficient. Pending bytes become canonical `ownership.json` only
after successful write, flush/fsync and close; incomplete receipts supply no canonical
ownership or pruning authority. The optional ownership upload uses only that canonical
path, warns if missing and cannot by itself fail the publication job.

This does not repair historical failed attempts or reconcile runner loss, timeouts, uncertain
publication responses, or failed/incomplete final verification. Those cases remain stopped
for release-manager reviewed manual handling; re-running does not repair an old recorded
failure. No general authenticated reconciliation protocol, retry or public-state editing/
deletion authority is introduced. Manual handling of these remaining cases still needs
preactivation review and qualification. These are source behaviors, not hosted/crypto proof.

Confirm the shared parser's final exact fields; unknown/missing/null values must refuse.
Do not derive policy, code/tool paths, trust anchors or ignored debt from a candidate archive.
Independently pinned signing tools are also unresolved; configure them, and the trust values
above, only through the reviewed shared/release contract, never a guessed app value or an
override from main. The unchanged stable `release.yml` forwards Windows/Azure
secret names from caller scope, while the nightly signer forwards none. Before activation,
verify the existing `release` environment holds the required names and that repository or
organization scopes do not expose signing copies to main. Environment-only missing values
must fail closed; repository/org-only values or same-name scope collisions are blockers,
not permission to copy keys or change settings. No secret names/scopes or protection settings
were audited by this source change; no secret values were read.

## Bounded integration and acceptance

1. The callers reference the same reviewed public preview commit in `uses` and
   `tooling-revision`. Activation is manual dispatch by a release manager only: never add a
   schedule, an automatic signer or another actor without its own review. Future shared pin
   updates require review of the exact public immutable source and interface. Do not claim
   untested source as a working pipeline.
   Do not temporarily call old same-run signing or use mutable `@main`/`@release` tooling.
   Keep compiler permissions contents and Actions read only, with no OIDC, attestation write,
   release environment or platform secrets. Only the isolated unsigned attester receives
   its required Actions-read, attestation-write and OIDC, never platform keys. Re-run the
   no-cache static check over **all** trusted jobs/callees, including setup-action cache options.
2. Register on main through normal review. The signer no longer rides the stable sync: it runs on
   `refs/heads/nightly-signer`, advanced only to reviewed main commits, following
   [the release trust-closure review](releasing.md#cutting-a-release).
   A release push can build/update a stable draft or refuse an already-published version;
   this source change authorizes no push, skip-CI workaround or dummy version bump.
3. Hosted **no-platform-key**, separately authorized tests must establish actual release
   dispatch context/permissions, queue retention/displacement, re-runs/current actor denial,
   exact attestation `invocationId` run/attempt and immutable reusable identity. Test wrong
   repository/ref/tag/event/actor, substituted run/attempt/artifact/transport ZIP/inner package,
   missing/expired artifacts and all digest/claim mismatches. Transport archive SHA, unsigned
   package SHA and final signed SHA are different roles, never interchangeable.
4. Candidate-v3 quality must prove exact C required latest runs/jobs and take the Sonar
   snapshot first. Require coherent latest main-C revision/SCM at collection boundaries,
   complete paginated branch issues scoped to reviewed issue types with count/end reread,
   and the same analysis's gate and exact reviewed debt fingerprints. Profiles, rules,
   scanner configuration and warnings are **trusted as resolved by the provider**, like
   main CI; configuration changes can hide issues. This is not immutable scan-inventory
   proof, full scan-scope enforcement or a warning-free-scan guarantee. The example has no
   `sonar.scope` field; its exact Sonar fields match the shared candidate-v3 parser.

   Debt may shrink only with analysis-driven FIXED lifecycle proof **and** independently
   qualified exact-C source-hash absence; missing/manual/REMOVED/unchanged-source proofs
   refuse. Cloud source-hash normalization remains unqualified, so a vanished approved debt
   item currently **refuses**, rather than counting absence as a fix. Qualify the real hash
   semantics and lifecycle APIs without new credentials before claiming that path works.

   Current main M may advance past C only by verified ancestry; C must strictly advance the
   previous authenticated publication P. Retained-snapshot revalidation currently refuses
   once the latest Sonar analysis advances past C: no qualified historical issue/proof API
   resolves that race. Do not promise publication survives a newer analysis. Repeat
   admission/pause/quality/ancestry/newest-run checks before keys and publication.
5. Separately authorize first native Windows signing/publication, signature/publisher/timestamp
   checks, Launcher install beside stable, launch/profile/version independence, rollback and
   uninstall preservation, and corrupt/unsigned/wrong-publisher rejection. No Linux/macOS
   coverage follows from Windows. Keep production/native/publication/prune switches off until
   their own acceptance. Nightly assets remain prerelease, `make_latest:false`, with separate
   nightly tags/reservations. Historical failures, uncertain/failed final verification and
   runner loss retain the [manual-handling limits described above](#post-publication-recovery-limitation);
   receipt-failure prevention does not supply general reconciliation.

Routine updates ride real stable windows, never automatic main-to-release copying. Pause or
cancel via existing nightly workflow controls without pushing release; urgent repairs need
one explicitly authorized bounded maintenance push. See the shared-identity incident note in
[the release guide](releasing.md#release-ref-nightly-preparation). No daily witness renewal or
per-run human approval is introduced by candidate-v3; provider gaps remain blockers.

## Linux Nightly launchers (on top of the Linux packaging PR)

A Nightly build refuses to start without its private profile, and refuses the stable
`FILMCRAFT_DATA_DIR` override, so every Linux format starts the GUI through the Launcher's literal
protocol `--launch-profile nightly-v1 --profile-root <dir> -- <files>`. The deb/rpm wrapper, the
AppImage's `AppRun`, the Arch handoff and the Flatpak use
`${XDG_DATA_HOME:-$HOME/.local/share}/filmcraft-nightly/profile` (outside stable's folders and
outside the installed copy, which the app's own fence would reject). The plain tar is the exception:
its `filmcraft` is craft-common-actions' shared launcher template, rendered for this app, whose
default root is `${XDG_DATA_HOME:-$HOME/.local/share}/ai.storyteller.filmcraft.nightly`. A
first-argument `--version` or `-V` still answers, and a first-argument `--launch-profile` pair is
forwarded unchanged; the packaged launchers also pass `--profile-root`, `--help` and `-h` through
untouched, which the plain tar's narrower template does not.

| Format | Shape |
|---|---|
| deb/rpm | GUI in `/usr/lib/filmcraft-nightly/filmcraft`, launcher at `/usr/bin/filmcraft-nightly`, CLI at `/usr/bin/filmcraft-nightly-cli` |
| AppImage | `AppRun` is a script (never a symlink): desktop integration plus the profile launch; `package.sh` switches its `DESK_ID` and `CHANNEL` for Nightly |
| Arch handoff | `bin/filmcraft-nightly` launcher, GUI in `bin/.filmcraft-nightly/` |
| Flatpak | GUI in `/app/libexec/filmcraft`, launcher in `/app/bin/filmcraft`; the Nightly bundle id is the only `FLATPAK_ID` the fence accepts |
| plain tar | `filmcraft` launcher beside `filmcraft-bin`, and `install.sh` installs both |

Nightly packages claim no MIME associations (desktop and metainfo), ship the channel icon, and keep
the `filmcraft/neural-voices` feature the official package builds with.
`packaging/linux/nightly-launch-test.sh`, `apprun-test.sh` and `nightly-arch-tar-test.sh` check the
launchers with argv recorders (packaging lint runs them).

## Local regression checks

`python3 packaging/nightly-release-tests.py` executes the **actual inline dispatcher** with
synthetic, locally generated event fixtures and a mocked HTTPS opener. It verifies fixed
routing, refusal before contact and no retry; it does not contact GitHub or prove admission,
cryptography, native signing, hosted concurrency, or the shared candidate-v3 implementation.
It checks that the policy example's platforms equal the producer list, that each row names its
fixed artifact kind and stable identity, that `.github/nightly-linux-signing-key.asc` holds exactly
one public primary key whose fingerprint is the policy's, and that both callers share one pin in
`uses` and `tooling-revision`.
It also checks the installed policy against the callers (paths and tooling revisions, the
release managers as the only manual actors in both places, platforms, candidate workflows and the
Sonar identity). The same suite covers all three callers for the no-cache/no-platform-secret
boundary, holds each enabled job to its exact context guard, and refuses any registered nightly
schedule. It also rejects changed root serialization: the fixed FilmCraft nightly group and
`cancel-in-progress: false` must remain literal, and a `queue` entry, when present, must be `max`.
Mutation regressions reject invalid queue values, cancellation and request-dependent groups. Packaging lint runs this suite in its
independent contents-read-only `nightly-source-contract` job, including config-only changes;
it does not rely on the shared Bash packaging hook to run the guard.
The suite also runs the `nightly-signer` trust-closure scan: no app workflow binds `environment:`,
only `nightly-sign.yml` calls `nightly-release.yml` at a 40-hex SHA, and the only caller of the
shared stable `release.yml` is `release.yml`.

The existing `.github/actionlint.yaml` suppresses only actionlint 1.7.12's anchored exact
unsupported-queue message for `.github/workflows/nightly-sign.yml`. This operator-requested
compatibility exception is not a blanket syntax waiver: other files/messages still fail,
and the mandatory source guard supplies the missing queue-value/serialization checks.
No tool fork or shared lint change is needed. This same-PR nonprivileged CI hook is the
narrow correction requested for the nightly source; stable/release execution is unchanged.
Run through the normal task allocator; report actionlint/PowerShell/native/hosted checks
separately and never treat a missing runtime as a pass.
