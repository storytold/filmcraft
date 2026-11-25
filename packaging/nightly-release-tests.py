#!/usr/bin/env python3
"""Synthetic dispatcher regression fixtures; original local work, not live API/crypto evidence."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import re
import tempfile
import textwrap
import base64
import hashlib
import unittest
from unittest.mock import patch
import urllib.error

ROOT = Path(__file__).resolve().parents[1]
DISPATCHER = ROOT / ".github/workflows/nightly-dispatch.yml"


def inline_scripts(path):
    """Read literal run blocks, not a substitute for YAML/actionlint validation."""
    return [textwrap.dedent(block) for block in re.findall(
        r"^        run: \|\n((?:          .*\n|\n)+)", path.read_text(), re.MULTILINE)]


def assert_nightly_serialization(source):
    """Fail closed unless this app's plain root concurrency block retains its contract.

    This is a deliberately narrow source guard, not a general YAML parser; actionlint
    still checks all other syntax. Expressions, aliases and alternate block shapes
    refuse rather than delegating queue-value validation to the old linter.
    """
    lines = source.splitlines()
    roots = [index for index, line in enumerate(lines) if line.startswith("concurrency:")]
    if len(roots) != 1 or lines[roots[0]] != "concurrency:":
        raise ValueError("one plain root concurrency block is required")
    values = {}
    for line in lines[roots[0] + 1:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not line[0].isspace():
            break
        row = re.fullmatch(r"  (group|cancel-in-progress|queue): ([^#]+?)(?: +#.*)?", line)
        if row is None or row[1] in values:
            raise ValueError("malformed or duplicate nightly concurrency entry")
        values[row[1]] = row[2].rstrip()
    allowed = {"group": "filmcraft-nightly-release", "cancel-in-progress": "false"}
    if "queue" in values:
        # Optional in the caller; when present it must keep GitHub's full queue, never coalesce.
        if values["queue"] != "max":
            raise ValueError("a nightly queue entry must be `max`")
        allowed["queue"] = "max"
    if values != allowed:
        raise ValueError("nightly serialization must use its stable group and no cancellation")


def job_conditions(jobs):
    """Each job-level `if:` as (block indicator or None, the whole expression YAML would read)."""
    lines = jobs.splitlines()
    found = []
    for index, line in enumerate(lines):
        head = re.fullmatch(r"    if:(?: (.*))?", line)
        if head is None:
            continue
        value = (head[1] or "").strip()
        continuation = []
        for follow in lines[index + 1:]:
            if follow.strip() and len(follow) - len(follow.lstrip(" ")) <= 4:
                break
            continuation.append(follow.strip())
        indicator = value if re.fullmatch(r"[>|][+-]?", value) else None
        parts = continuation if indicator else [value] + continuation
        found.append((indicator, " ".join(part for part in parts if part)))
    return found


def reviewed_manual_actors(caller_source, policy):
    """The one reviewed manual-actor list, identical in the producer caller and the policy."""
    rows = re.findall(r"(?m)^      manual-actors: '(\[[^']*\])'$", caller_source)
    if len(rows) != 1:
        raise ValueError("exactly one literal manual-actors list is required")
    caller = json.loads(rows[0])
    if caller != policy["manual_actors"]:
        raise ValueError("caller and policy must grant the same actors in the same order")
    if caller != RELEASE_MANAGERS:
        raise ValueError("manual actors must be exactly the reviewed release managers")
    if caller != sorted(caller, key=str.lower) or len({login.lower() for login in caller}) != len(caller):
        raise ValueError("manual actors must be distinct and sorted case-insensitively")
    if set(caller) & {login for login in policy["automatic_signer_actors"]}:
        raise ValueError("manual and automatic actors overlap")
    return caller


def caller_pin(name):
    return re.findall(r"(?m)^      tooling-revision: ([0-9a-f]{40})$",
                      (ROOT / ".github/workflows" / name).read_text())[0]


NIGHTLY_PLATFORMS = ("windows-x86_64", "linux-x86_64", "macos-aarch64")
# Each platform's one trust field in policy v3, and the public stable identity it must name.
NIGHTLY_TRUST = {
    "windows-x86_64": ("publisher_subject",
                       "CN=Learning Machines Inc, O=Learning Machines Inc, L=Atlanta, S=Georgia, C=US"),
    "linux-x86_64": ("fingerprint", "2B76741B414AAD6CD33E631213A005C0B9D7C3ED"),
    "macos-aarch64": ("apple_team_id", "DJ6XS33FX8"),
}
LINUX_KEY = ROOT / ".github/nightly-linux-signing-key.asc"
# The installed v3 policy is Dylan's reviewed manual activation: manual dispatch only, no schedule.
INSTALLED_POLICY = ROOT / ".github/nightly-signing-policy.json"

# Mirrors the storytold release-managers team (id 19837241) minus echelon-robot.
# Update by PR, in the caller and the policy, when the team changes.
RELEASE_MANAGERS = ["bflatastic", "dylanmtaylor", "echelon", "FredApps", "skylartaylor"]


def openpgp_primary_fingerprints(armored):
    """Return the v4 fingerprints of every primary public-key packet in one armored block.

    A narrow reader for the committed public key, not a general OpenPGP parser: anything
    other than one well-formed public-key block refuses.
    """
    lines = armored.strip().splitlines()
    if lines[:1] != ["-----BEGIN PGP PUBLIC KEY BLOCK-----"] or lines[-1:] != ["-----END PGP PUBLIC KEY BLOCK-----"]:
        raise ValueError("exactly one armored public key block is required")
    body = lines[1:-1]
    if "" not in body:
        raise ValueError("armor header separator missing")
    body = body[body.index("") + 1:]
    if not body or not body[-1].startswith("="):
        raise ValueError("armor checksum missing")
    data = base64.b64decode("".join(body[:-1]), validate=True)
    fingerprints, offset = [], 0
    while offset < len(data):
        header = data[offset]
        if not header & 0x80:
            raise ValueError("invalid packet header")
        if header & 0x40:
            tag, offset = header & 0x3F, offset + 1
            first = data[offset]
            if first < 192:
                length, offset = first, offset + 1
            elif first < 224:
                length, offset = ((first - 192) << 8) + data[offset + 1] + 192, offset + 2
            elif first == 255:
                length, offset = int.from_bytes(data[offset + 1:offset + 5], "big"), offset + 5
            else:
                raise ValueError("partial packet lengths are not a public key")
        else:
            tag, kind, offset = (header >> 2) & 0x0F, header & 0x03, offset + 1
            if kind == 3:
                raise ValueError("indeterminate packet length")
            size = 1 << kind
            length, offset = int.from_bytes(data[offset:offset + size], "big"), offset + size
        packet = data[offset:offset + length]
        if len(packet) != length:
            raise ValueError("truncated packet")
        if tag == 5 or tag == 7:
            raise ValueError("secret key material must never be committed")
        if tag == 6:
            if packet[:1] != b"\x04":
                raise ValueError("only v4 primary keys are expected")
            fingerprints.append(hashlib.sha1(b"\x99" + length.to_bytes(2, "big") + packet).hexdigest().upper())
        offset += length
    return fingerprints


def producer_platforms(source):
    """Read the one producer job's literal platform list; a narrow source guard, not YAML.

    One producer run builds every listed platform for one all-or-nothing release, so the
    list must be a compact JSON list of distinct known platforms that keeps Windows.
    """
    rows = re.findall(r"(?m)^      platforms: (.*)$", source)
    if len(rows) != 1:
        raise ValueError("exactly one producer platform list is required")
    literal = re.fullmatch(r"'(\[[^']*\])'", rows[0])
    if literal is None:
        raise ValueError("the platform list must be one single-quoted literal")
    platforms = json.loads(literal[1])
    if not isinstance(platforms, list) or json.dumps(platforms, separators=(",", ":")) != literal[1]:
        raise ValueError("the platform list must be compact JSON")
    if len(set(platforms)) != len(platforms) or not set(platforms) <= set(NIGHTLY_PLATFORMS):
        raise ValueError("platforms must be distinct known nightly targets")
    if "windows-x86_64" not in platforms:
        raise ValueError("the Windows row stays in the producer")
    return platforms


class Response:
    status = 204

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


class DispatchBoundary(unittest.TestCase):
    def setUp(self):
        # All generated metadata is synthetic, including the presence-only mock token.
        self.event = {"action": "completed", "repository": {"full_name": "storytold/filmcraft"},
                      "workflow_run": {"id": 71, "run_attempt": 1, "status": "completed",
                                       "conclusion": "success", "event": "schedule",
                                       "path": ".github/workflows/nightly.yml", "head_branch": "main",
                                       "head_repository": {"full_name": "storytold/filmcraft"}}}
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.event_path = Path(self.temp.name) / "event.json"
        self.env = {"GITHUB_REPOSITORY": "storytold/filmcraft", "GITHUB_EVENT_NAME": "workflow_run",
                    "GITHUB_EVENT_PATH": str(self.event_path), "GH_TOKEN": "synthetic-not-a-token"}
        scripts = inline_scripts(DISPATCHER)
        if len(scripts) != 1:
            raise RuntimeError("dispatcher must have one locally exercisable literal script")
        self.program = compile(scripts[0], str(DISPATCHER), "exec")

    def execute(self, event=None, env=None, failure=None, response=None):
        self.event_path.write_text(json.dumps(self.event if event is None else event))
        scope = {}
        with patch.dict(os.environ, self.env if env is None else env, clear=True), \
                patch("urllib.request.build_opener") as opener, \
                contextlib.redirect_stdout(io.StringIO()):
            opener.return_value.open.return_value = response or Response()
            opener.return_value.open.side_effect = failure
            try:
                exec(self.program, scope)
            except Exception as error:
                return opener, scope, error
        return opener, scope, None

    def assert_refused(self, event=None, env=None):
        opener, _, error = self.execute(event=event, env=env)
        self.assertIsNotNone(error, "invalid transport must refuse")
        opener.assert_not_called()

    def test_only_fixed_release_destination_and_numeric_request_are_transported(self):
        # Caller-selected source/ref/digest fields have no effect on the transmitted request.
        self.event["workflow_run"].update({"ref": "main", "artifact_id": 999,
                                           "sha256": "untrusted", "tooling": "untrusted"})
        opener, _, error = self.execute()
        self.assertIsNone(error)
        opener.return_value.open.assert_called_once()
        request = opener.return_value.open.call_args.args[0]
        self.assertEqual(request.full_url,
                         "https://api.github.com/repos/storytold/filmcraft/actions/workflows/nightly-sign.yml/dispatches")
        self.assertEqual(request.get_method(), "POST")
        self.assertEqual(json.loads(request.data), {"ref": "release", "inputs": {
            "producer_run_id": "71", "producer_run_attempt": "1"}})
        self.assertGreater(opener.return_value.open.call_args.kwargs["timeout"], 0)

    def test_wrong_top_level_context_refuses_before_network(self):
        for key, value in (("GITHUB_REPOSITORY", "other/filmcraft"),
                           ("GITHUB_EVENT_NAME", "workflow_dispatch")):
            with self.subTest(key=key):
                self.assert_refused(env={**self.env, key: value})

    def test_foreign_or_noncompleted_events_refuse_before_network(self):
        for key, value in (("action", "requested"),
                           ("repository", {"full_name": "other/filmcraft"})):
            with self.subTest(key=key):
                self.assert_refused(event={**self.event, key: value})

    def test_wrong_producer_identity_status_and_manual_retries_refuse(self):
        for key, value in (("head_repository", {"full_name": "other/filmcraft"}),
                           ("head_branch", "release"), ("head_branch", "refs/tags/main"),
                           ("path", ".github/workflows/release.yml"), ("status", "in_progress"),
                           ("conclusion", "failure"), ("conclusion", "cancelled"),
                           ("event", "workflow_dispatch"), ("event", "pull_request"),
                           ("run_attempt", 2)):
            event = copy.deepcopy(self.event)
            event["workflow_run"][key] = value
            with self.subTest(key=key, value=value):
                self.assert_refused(event=event)

    def test_ids_are_not_coerced_from_strings_booleans_or_fractional_values(self):
        for key in ("id", "run_attempt"):
            for value in (None, False, True, 0, -1, 1.5, "1", "1;evil", [], {}):
                event = copy.deepcopy(self.event)
                event["workflow_run"][key] = value
                with self.subTest(key=key, value=value):
                    self.assert_refused(event=event)

    def test_missing_metadata_never_contacts_api(self):
        for key in self.event["workflow_run"]:
            event = copy.deepcopy(self.event)
            del event["workflow_run"][key]
            with self.subTest(key=key):
                self.assert_refused(event=event)

    def test_redirect_handler_refuses_instead_of_forwarding_authorization(self):
        _, scope, error = self.execute()
        self.assertIsNone(error)
        with self.assertRaisesRegex(RuntimeError, "redirects are forbidden"):
            scope["NoRedirect"]().redirect_request(None, None, 302, "found", {}, "https://untrusted.invalid")

    def test_failed_http_request_is_not_retried_or_reported_successful(self):
        for error in (TimeoutError("synthetic timeout"), urllib.error.URLError("synthetic failure")):
            with self.subTest(error=type(error).__name__):
                opener, _, caught = self.execute(failure=error)
                self.assertIs(caught, error)
                opener.return_value.open.assert_called_once()
        response = Response()
        response.status = 202
        opener, _, error = self.execute(response=response)
        self.assertIsNotNone(error)
        opener.return_value.open.assert_called_once()


class TrustedSourcePreparation(unittest.TestCase):
    def test_sonar_classifies_only_the_exact_packaging_fixture_runners(self):
        source = (ROOT / "sonar-project.properties").read_text()
        self.assertEqual(re.findall(r"(?m)^sonar.sources=(.*)$", source), ["."])
        self.assertEqual(re.findall(r"(?m)^sonar.tests=(.*)$", source), ["packaging"])
        inclusions = re.findall(r"(?m)^sonar.test.inclusions=(.*)$", source)
        self.assertEqual(len(inclusions), 1)
        entries = inclusions[0].split(",")
        expected = {"packaging/nightly-tests.py", "packaging/nightly-archive-tests.py",
                    "packaging/macos/nightly_plist_tests.py", "packaging/macos/nightly_sign_tests.py",
                    "packaging/windows/nightly-package-tests.ps1",
                    "packaging/windows/nightly-signing-tests.ps1", "packaging/nightly-release-tests.py"}
        self.assertEqual(set(entries), expected, "classify exact fixtures, never production helpers or wildcard scope")
        self.assertEqual(len(entries), len(expected), "duplicate fixture classification")

    def test_policy_example_keeps_explicit_v3_and_unresolved_authority(self):
        policy = json.loads((ROOT / "docs/nightly-signing-policy.example.json").read_text())
        expected = {"schema", "repository", "repository_id", "producer_workflow_id",
                    "producer_workflow_path", "producer_tooling_revision", "producer_workflow_ref",
                    "signer_workflow_path", "signer_tooling_revision", "release_branch", "manual_actors",
                    "automatic_signer_actors", "max_age_seconds", "candidate_policy",
                    "max_artifact_bytes", "platforms"}
        self.assertEqual(set(policy), expected, "no single-platform v2 fields or implicit migration")
        self.assertEqual(policy["schema"], "craft-nightly-release-policy/v3")
        self.assertEqual(policy["manual_actors"], [])
        self.assertEqual(policy["automatic_signer_actors"], [])
        for key in ("repository_id", "producer_workflow_id", "producer_tooling_revision",
                    "producer_workflow_ref", "signer_tooling_revision", "max_age_seconds",
                    "max_artifact_bytes"):
            with self.subTest(unresolved=key):
                self.assertIsNone(policy[key], "the example must not invent production authority")

    def test_policy_platforms_are_exactly_what_the_producer_builds(self):
        policy = json.loads((ROOT / "docs/nightly-signing-policy.example.json").read_text())
        built = producer_platforms((ROOT / ".github/workflows/nightly.yml").read_text())
        self.assertEqual(built, list(NIGHTLY_PLATFORMS), "one release carries Windows, Linux and macOS")
        self.assertEqual(set(policy["platforms"]), set(built), "a built platform without policy, or the reverse")
        for platform, row in policy["platforms"].items():
            field, _ = NIGHTLY_TRUST[platform]
            with self.subTest(platform=platform):
                self.assertEqual(set(row), {"unsigned_artifact_name", "trust"})
                self.assertEqual(set(row["trust"]), {field}, "each platform has only its own trust anchor")

    def test_candidate_example_has_exact_v3_fields_without_provider_scope_claim(self):
        candidate = json.loads((ROOT / "docs/nightly-signing-policy.example.json").read_text())["candidate_policy"]
        self.assertEqual(set(candidate), {"schema", "epoch", "repository", "branch", "required_workflows", "sonar"})
        self.assertEqual(candidate["schema"], "craft-nightly-candidate-policy/v3")
        self.assertIsNone(candidate["required_workflows"])
        self.assertEqual(set(candidate["sonar"]), {"organization", "project", "app_id", "check_name",
                         "details_url", "issue_types", "accepted_conditions", "required_conditions", "debt"})
        self.assertTrue(all(value is None for value in candidate["sonar"].values()), "real identity/debt is unresolved")

    def test_one_producer_and_one_signer_job_carry_every_platform(self):
        for name, job in (("nightly.yml", "unsigned"), ("nightly-sign.yml", "release")):
            jobs = (ROOT / ".github/workflows" / name).read_text().split("\njobs:\n", 1)[1]
            with self.subTest(workflow=name):
                self.assertEqual(re.findall(r"(?m)^  ([^\s#][^:]*):", jobs), [job],
                                 "a platform is a list entry, not a separately activatable job")
        signer = (ROOT / ".github/workflows/nightly-sign.yml").read_text()
        self.assertNotRegex(signer, r"(?m)^\s*platforms?:", "the release policy, not the caller, admits platforms")

    def test_malformed_or_narrowed_platform_lists_are_rejected(self):
        source = (ROOT / ".github/workflows/nightly.yml").read_text()
        current = "platforms: '[\"windows-x86_64\",\"linux-x86_64\",\"macos-aarch64\"]'"
        self.assertIn(current, source)
        for new in ("platforms: '[\"linux-x86_64\"]'",
                    "platforms: '[\"linux-x86_64\",\"macos-aarch64\"]'",
                    "platforms: '[\"windows-x86_64\",\"linux-x86_64\",\"linux-x86_64\"]'",
                    "platforms: '[\"windows-x86_64\",\"macos-x86_64\"]'",
                    "platforms: '[\"windows-x86_64\",\"linux-arm64\"]'",
                    "platforms: '[\"windows-x86_64\", \"linux-x86_64\"]'",
                    "platforms: [\"windows-x86_64\",\"linux-x86_64\"]",
                    "platforms: ${{ vars.NIGHTLY_PLATFORMS }}",
                    "platforms: '{\"windows-x86_64\":1}'",
                    current + "\n      " + current):
            with self.subTest(change=new), self.assertRaises(ValueError):
                producer_platforms(source.replace(current, new))
        with self.assertRaises(ValueError):
            producer_platforms(source.replace("      " + current + "\n", ""))

    def test_no_nightly_schedule_is_registered(self):
        for name, event in (("nightly.yml", "workflow_dispatch"),
                            ("nightly-dispatch.yml", "workflow_run"),
                            ("nightly-sign.yml", "workflow_dispatch")):
            source = (ROOT / ".github/workflows" / name).read_text()
            events = source.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
            triggers = [line for line in events.splitlines() if line.startswith("  ")
                        and not line.startswith("   ") and line.strip() and not line.lstrip().startswith("#")]
            with self.subTest(workflow=name):
                self.assertEqual(triggers, [f"  {event}:"], "schedule/activation needs explicit review")

    def test_signer_serialization_contract_is_enforced(self):
        assert_nightly_serialization((ROOT / ".github/workflows/nightly-sign.yml").read_text())

    def test_weakened_or_malformed_serialization_is_rejected(self):
        source = (ROOT / ".github/workflows/nightly-sign.yml").read_text()
        for old, new in (
            ("cancel-in-progress: false", "cancel-in-progress: true"),
            ("cancel-in-progress: false", "cancel-in-progress: ${{ github.event_name == 'push' }}"),
            ("cancel-in-progress: false\n", ""),
            ("group: filmcraft-nightly-release", "group: ${{ github.run_id }}"),
            ("group: filmcraft-nightly-release", "group: release-${{ github.ref }}"),
            ("concurrency:\n", "concurrency: ${{ github.run_id }}\n"),
            ("concurrency:\n", "  concurrency:\n"),
        ):
            with self.subTest(change=new), self.assertRaises(ValueError):
                assert_nightly_serialization(source.replace(old, new))
        # The flavor's stricter form adds `queue: max`; a caller may omit it, never weaken it.
        queued = source.replace("cancel-in-progress: false", "cancel-in-progress: false\n  queue: max")
        assert_nightly_serialization(queued)
        for value in ("queue: single", "queue: invalid", "queue: ${{ github.run_id }}", "queue:"):
            with self.subTest(queue=value), self.assertRaises(ValueError):
                assert_nightly_serialization(queued.replace("queue: max", value))
        duplicated = queued.replace("cancel-in-progress: false", "cancel-in-progress: false\n  queue: max")
        with self.assertRaises(ValueError):
            assert_nightly_serialization(duplicated)

    def test_no_cache_actions_or_cache_setup_options_in_app_trusted_closure(self):
        # Required R3 static guard for app workflows only; activation must extend
        # this check to every reviewed immutable shared trusted callee.
        for name in ("nightly.yml", "nightly-sign.yml", "nightly-dispatch.yml"):
            source = (ROOT / ".github/workflows" / name).read_text()
            executable = "\n".join(line for line in source.splitlines()
                                   if not line.lstrip().startswith("#"))
            with self.subTest(workflow=name):
                self.assertNotRegex(executable, r"(?im)^\s*(?:-\s*)?uses:\s*[^\n]*cache")
                self.assertNotRegex(executable, r"(?im)^\s*[^\s:#]*cache[^\s:#]*:")
                self.assertNotRegex(executable, r"(?im)^\s*(?:environment|secrets):")
                self.assertNotIn("secrets.", executable)


class NightlyEnabledCallers(unittest.TestCase):
    """Producer and signer run only in their exact context, for the reviewed manual actors, with no schedule."""

    GUARDS = {
        "nightly.yml": "github.repository == 'storytold/filmcraft' && github.ref == 'refs/heads/main' && "
                       "github.ref_type == 'branch' && (github.event_name == 'schedule' || "
                       "github.event_name == 'workflow_dispatch')",
        "nightly-sign.yml": "github.repository == 'storytold/filmcraft' && github.event_name == 'workflow_dispatch' && "
                            "github.ref == 'refs/heads/release' && github.ref_type == 'branch'",
    }

    def test_enabled_jobs_keep_exact_context_guards(self):
        for name, expected in self.GUARDS.items():
            jobs = (ROOT / ".github/workflows" / name).read_text().split("\njobs:\n", 1)[1]
            guards = job_conditions(jobs)
            with self.subTest(workflow=name):
                self.assertEqual(len(guards), 1)
                indicator, expression = guards[0]
                self.assertEqual(indicator, ">-")
                self.assertEqual(" ".join(expression.split()), expected, "guards may only change with review")

    def test_continuation_lines_of_any_scalar_form_reach_the_guard_checks(self):
        for name in ("nightly.yml", "nightly-sign.yml"):
            source = (ROOT / ".github/workflows" / name).read_text()
            expected = self.GUARDS[name]
            guard = re.search(r"(?m)^    if: >-\n(?:      [^\n]*\n)+", source)[0]
            for replacement in (guard + "     || true\n", "    if: " + expected + "\n      || true\n"):
                jobs = source.replace(guard, replacement).split("\njobs:\n", 1)[1]
                with self.subTest(workflow=name, replacement=replacement[-30:]):
                    (indicator, expression), = job_conditions(jobs)
                    self.assertFalse(indicator == ">-" and " ".join(expression.split()) == expected)

    def test_manual_actors_are_the_release_managers_in_caller_and_policy(self):
        source = (ROOT / ".github/workflows/nightly.yml").read_text()
        policy = json.loads(INSTALLED_POLICY.read_text())
        self.assertEqual(reviewed_manual_actors(source, policy), RELEASE_MANAGERS)
        self.assertEqual(policy["automatic_signer_actors"], [], "no automatic signer without a schedule")

    def test_an_actor_outside_the_release_managers_is_refused(self):
        source = (ROOT / ".github/workflows/nightly.yml").read_text()
        policy = json.loads(INSTALLED_POLICY.read_text())
        caller = json.dumps(RELEASE_MANAGERS, separators=(",", ":"))
        widened = sorted(RELEASE_MANAGERS + ["echelon-robot"], key=str.lower)
        for change in ("caller", "policy", "both", "order", "case"):
            mutated_source, mutated_policy = source, copy.deepcopy(policy)
            if change in ("caller", "both"):
                mutated_source = source.replace(caller, json.dumps(widened, separators=(",", ":")))
            if change in ("policy", "both"):
                mutated_policy["manual_actors"] = widened
            if change == "order":
                mutated_policy["manual_actors"] = list(reversed(RELEASE_MANAGERS))
            if change == "case":
                mutated_policy["manual_actors"] = [login.lower() for login in RELEASE_MANAGERS]
            with self.subTest(change=change), self.assertRaises(ValueError):
                reviewed_manual_actors(mutated_source, mutated_policy)


def push_block(source):
    """The whole `push:` mapping, blank lines and nested keys included.

    A blank line does not end a YAML mapping, so the block is delimited by indentation rather than
    by contiguous non-empty lines; a path filter after a blank line stays visible to the check.
    """
    lines = source.splitlines()
    for index, line in enumerate(lines):
        if re.fullmatch(r"  push:.*", line):
            block = []
            for follow in lines[index + 1:]:
                if follow.strip() and not follow.startswith("    "):
                    break
                block.append(follow)
            return "\n".join(block)
    return None


class NightlyInstalledPolicy(unittest.TestCase):
    """The installed v3 policy matches the callers, the repository and its documented sources."""

    def setUp(self):
        self.assertFalse(INSTALLED_POLICY.is_symlink())
        self.policy = json.loads(INSTALLED_POLICY.read_text())

    def test_exact_v3_fields_and_repository_identity(self):
        expected = {"schema", "repository", "repository_id", "producer_workflow_id",
                    "producer_workflow_path", "producer_tooling_revision", "producer_workflow_ref",
                    "signer_workflow_path", "signer_tooling_revision", "release_branch", "manual_actors",
                    "automatic_signer_actors", "max_age_seconds", "candidate_policy",
                    "max_artifact_bytes", "platforms"}
        self.assertEqual(set(self.policy), expected)
        self.assertEqual(self.policy["schema"], "craft-nightly-release-policy/v3")
        self.assertEqual(self.policy["repository"], "storytold/filmcraft")
        self.assertEqual(self.policy["release_branch"], "release")
        self.assertIs(type(self.policy["repository_id"]), int)
        self.assertGreater(self.policy["repository_id"], 0)
        # producer_workflow_id is null until nightly.yml is registered on main; once set, a positive int.
        if self.policy["producer_workflow_id"] is not None:
            self.assertIs(type(self.policy["producer_workflow_id"]), int)
            self.assertGreater(self.policy["producer_workflow_id"], 0)
            self.assertNotEqual(self.policy["producer_workflow_id"],
                                self.policy["candidate_policy"]["required_workflows"][0]["workflow_id"],
                                "the producer is not its own quality workflow")

    def test_paths_and_tooling_match_the_callers(self):
        pin = caller_pin("nightly.yml")
        self.assertEqual(caller_pin("nightly-sign.yml"), pin)
        self.assertEqual(self.policy["producer_workflow_path"], ".github/workflows/nightly.yml")
        self.assertEqual(self.policy["signer_workflow_path"], ".github/workflows/nightly-sign.yml")
        self.assertEqual(self.policy["producer_tooling_revision"], pin)
        self.assertEqual(self.policy["signer_tooling_revision"], pin)
        self.assertEqual(self.policy["producer_workflow_ref"],
                         "storytold/craft-common-actions/.github/workflows/nightly-unsigned.yml@" + pin)

    def test_platforms_equal_the_producer(self):
        built = producer_platforms((ROOT / ".github/workflows/nightly.yml").read_text())
        self.assertEqual(built, list(NIGHTLY_PLATFORMS))
        self.assertEqual(set(self.policy["platforms"]), set(built))

    def test_bounds(self):
        self.assertIs(type(self.policy["max_age_seconds"]), int)
        self.assertTrue(0 < self.policy["max_age_seconds"] <= 604800)
        self.assertIs(type(self.policy["max_artifact_bytes"]), int)
        self.assertTrue(0 < self.policy["max_artifact_bytes"] <= 16 * 1024 ** 3)

    def test_candidate_workflows_run_on_every_main_push(self):
        candidate = self.policy["candidate_policy"]
        self.assertEqual(set(candidate), {"schema", "epoch", "repository", "branch", "required_workflows", "sonar"})
        self.assertEqual(candidate["schema"], "craft-nightly-candidate-policy/v3")
        self.assertEqual((candidate["repository"], candidate["branch"]), ("storytold/filmcraft", "main"))
        self.assertTrue(isinstance(candidate["epoch"], str) and 0 < len(candidate["epoch"]) <= 128)
        rows = candidate["required_workflows"]
        self.assertTrue(rows)
        for row in rows:
            with self.subTest(workflow=row["workflow_path"]):
                self.assertEqual(set(row), {"workflow_id", "workflow_path", "jobs"})
                self.assertIs(type(row["workflow_id"]), int)
                self.assertNotEqual(row["workflow_path"], self.policy["producer_workflow_path"])
                source = (ROOT / row["workflow_path"]).read_text()
                push = push_block(source)
                self.assertIsNotNone(push, "a required workflow must run on main pushes")
                self.assertIn("branches: [main]", push)
                self.assertNotRegex(push, r"(?m)^\s+paths(?:-ignore)?:", "a path filter would leave candidates without a run")
                names = [job["name"] for job in row["jobs"]]
                self.assertEqual(len(names), len(set(names)))
                for job in row["jobs"]:
                    self.assertEqual(set(job), {"name", "app_id", "applicability"})
                    self.assertEqual((job["app_id"], job["applicability"]), (15368, "required"))

    def test_a_path_filter_after_a_blank_line_is_still_rejected(self):
        workflow = "on:\n  push:\n    branches: [main]\n\n    paths: ['src/**']\n  workflow_dispatch: {}\n"
        block = push_block(workflow)
        self.assertIsNotNone(block)
        self.assertIn("branches: [main]", block)
        self.assertRegex(block, r"(?m)^\s+paths(?:-ignore)?:")
        for ignored in ("paths: ['src/**']", "paths-ignore: ['docs/**']"):
            with self.subTest(ignored=ignored):
                variant = workflow.replace(ignored.split(":")[0] + ": ['src/**']", ignored)
                self.assertRegex(push_block(variant), r"(?m)^\s+paths(?:-ignore)?:",
                                 "a quoted or unquoted filter must not hide behind a blank line")

    def test_sonar_identity_and_reviewed_scope(self):
        sonar = self.policy["candidate_policy"]["sonar"]
        properties = dict(line.split("=", 1) for line in (ROOT / "sonar-project.properties").read_text().splitlines()
                          if line and not line.startswith("#"))
        self.assertEqual(sonar["organization"], properties["sonar.organization"])
        self.assertEqual(sonar["project"], properties["sonar.projectKey"])
        self.assertEqual((sonar["app_id"], sonar["check_name"]), (12526, "SonarCloud Code Analysis"))
        self.assertEqual(sonar["details_url"], "https://sonarcloud.io/dashboard?id=" + sonar["project"] + "&branch=main")
        accepted = {"bugs": "BUG", "vulnerabilities": "VULNERABILITY", "code_smells": "CODE_SMELL",
                    "security_rating": "VULNERABILITY", "new_security_rating": "VULNERABILITY"}
        self.assertEqual({accepted[c["metric"]] for c in sonar["accepted_conditions"]}, set(sonar["issue_types"]))
        metrics = [c["metric"] for c in sonar["accepted_conditions"] + sonar["required_conditions"]]
        self.assertEqual(len(metrics), len(set(metrics)))
        keys = [item["key"] for item in sonar["debt"]]
        self.assertEqual(len(keys), len(set(keys)))
        for item in sonar["debt"]:
            with self.subTest(issue=item["key"]):
                self.assertEqual(set(item), {"key", "rule", "component", "type", "severity", "hash"})
                self.assertTrue(item["component"].startswith(sonar["project"] + ":"))
                self.assertIn(item["type"], sonar["issue_types"])
                self.assertRegex(item["hash"], r"^[0-9a-f]{32}$")


class NightlyTrustAnchors(unittest.TestCase):
    """The public stable identities each platform's signed nightly must carry (all three platforms)."""

    def setUp(self):
        self.policy = json.loads(INSTALLED_POLICY.read_text())

    def test_each_platform_row_names_its_fixed_artifact_kind_and_stable_identity(self):
        self.assertEqual(set(self.policy["platforms"]), set(NIGHTLY_TRUST))
        for platform, (field, value) in NIGHTLY_TRUST.items():
            row = self.policy["platforms"][platform]
            with self.subTest(platform=platform):
                self.assertEqual(row["unsigned_artifact_name"], f"nightly-unsigned-{platform}",
                                 "the artifact kind is fixed by platform, never a caller pattern")
                self.assertEqual(row["trust"], {field: value}, "trust must be stable's public identity")

    def test_trust_values_have_the_shapes_the_shared_signer_accepts(self):
        rows = self.policy["platforms"]
        self.assertRegex(rows["macos-aarch64"]["trust"]["apple_team_id"], r"^[A-Z0-9]{10}$")
        self.assertRegex(rows["linux-x86_64"]["trust"]["fingerprint"], r"^[0-9A-F]{40}$")
        subject = rows["windows-x86_64"]["trust"]["publisher_subject"]
        self.assertTrue(0 < len(subject) <= 2048 and not set(subject) & {"\r", "\n", "\0"})

    def test_committed_linux_key_is_exactly_the_one_stable_public_key(self):
        self.assertFalse(LINUX_KEY.is_symlink(), "the key file must be a regular committed file")
        armored = LINUX_KEY.read_text(encoding="ascii")
        self.assertEqual(armored.count("-----BEGIN PGP"), 1, "exactly one armored block")
        self.assertEqual(openpgp_primary_fingerprints(armored), [NIGHTLY_TRUST["linux-x86_64"][1]],
                         "exactly one primary key, and it is the stable release key the policy trusts")

    def test_key_reader_refuses_secret_or_extra_material(self):
        armored = LINUX_KEY.read_text(encoding="ascii")
        for change in (armored.replace("PUBLIC KEY BLOCK", "PRIVATE KEY BLOCK"),
                       armored.replace("-----END PGP PUBLIC KEY BLOCK-----\n", ""),
                       "Comment: extra\n" + armored):
            with self.subTest(change=change[:40]), self.assertRaises(ValueError):
                openpgp_primary_fingerprints(change)

    def test_both_callers_use_one_shared_pin_in_uses_and_tooling_revision(self):
        pins = set()
        for name, callee in (("nightly.yml", "nightly-unsigned.yml"), ("nightly-sign.yml", "nightly-release.yml")):
            source = (ROOT / ".github/workflows" / name).read_text()
            uses = re.findall(r"(?m)^    uses: storytold/craft-common-actions/\.github/workflows/"
                              + re.escape(callee) + r"@(\S+)$", source)
            tooling = re.findall(r"(?m)^      tooling-revision: (\S+)$", source)
            with self.subTest(workflow=name):
                self.assertEqual(len(uses), 1)
                self.assertEqual(tooling, uses, "the defining workflow SHA and tooling input must match")
            pins.update(uses)
        self.assertEqual(len(pins), 1, "producer and signer must pin the same shared commit")
        self.assertRegex(pins.pop(), r"^[0-9a-f]{40}$", "pin a full immutable commit, never a branch or placeholder")


class LinuxNightlyLaunchers(unittest.TestCase):
    """Source contracts for the Linux Nightly launchers (argv behaviour: the bash suites)."""

    PROFILE = "${XDG_DATA_HOME:-${HOME:-}/.local/share}/filmcraft-nightly/profile"

    def launchers(self):
        package = (ROOT / "packaging/linux/package.sh").read_text()
        heredocs = re.findall(r"<<'WRAPPER'\n(.*?)\nWRAPPER\n", package, re.DOTALL)
        self.assertGreaterEqual(len(heredocs), 1, "the deb/rpm Nightly wrapper is missing")
        files = {}
        for name, path in (("tarball", "packaging/linux/nightly-launch.sh"),
                           ("arch", "packaging/linux/nightly-arch-launch.sh"),
                           ("apprun", "packaging/linux/AppRun"),
                           ("flatpak", "packaging/linux/flatpak-bundle.sh")):
            files[name] = (ROOT / path).read_text()
        flatpak = re.findall(r"<<'WRAPPER'\n(.*?)\nWRAPPER\n", files["flatpak"], re.DOTALL)
        self.assertEqual(len(flatpak), 1, "the Flatpak Nightly wrapper is missing")
        files["flatpak"] = flatpak[0]
        files["deb/rpm"] = "\n".join(heredocs)
        return files

    def test_every_linux_launcher_starts_the_gui_with_the_launcher_profile(self):
        for name, source in self.launchers().items():
            with self.subTest(launcher=name):
                self.assertIn("--launch-profile nightly-v1", source)
                self.assertIn(self.PROFILE, source, "one profile root, outside stable and the install")
                self.assertIn('case "${1-}" in', source)
                self.assertIn("--profile-root | --version | -V | --help | -h", source)
                # A caller-written `--` is stripped: the launcher's own separator must never
                # arrive at the app as a file name.
                self.assertIn('if [ "${1-}" = -- ]; then shift; fi', source)

    def test_no_linux_wrapper_routes_the_profile_through_the_environment(self):
        # The flavor refuses FILMCRAFT_DATA_DIR with a nightly profile: a wrapper that exported it
        # would make its own package exit 2.
        offenders = [path.name for path in sorted((ROOT / "packaging/linux").iterdir())
                     if path.suffix in {".sh", ".yml", ".yaml"} and "FILMCRAFT_DATA_DIR" in path.read_text(errors="ignore")]
        self.assertEqual(offenders, [], "the Launcher owns the Nightly root, not the environment")

    def test_the_package_keeps_the_gui_off_path_and_the_mediatypes_off_the_entry(self):
        package = (ROOT / "packaging/linux/package.sh").read_text()
        self.assertIn("/usr/lib/filmcraft-nightly/filmcraft", package)
        self.assertEqual(package.count("'/<mediatype>/d'"), 2, "the deb/rpm and AppImage metainfo")
        self.assertIn("--features filmcraft/neural-voices", package)
        self.assertIn("--features filmcraft/neural-voices", (ROOT / "packaging/nightly-common.sh").read_text(),
                      "the Nightly tar must keep the voices the official package builds with")
        self.assertIn("--features filmcraft/neural-voices", (ROOT / "packaging/windows/nightly.ps1").read_text(),
                      "the Windows Nightly must keep the voices the official package builds with")
        self.assertIn('Icon=$NIGHTLY_DESK_ID', package)
        # The drop commands are a prefix of the first sed expression: `sed "$DROP_LIB_TREE" -e ...`
        # reads an empty or command-shaped argument as an input file and dies.
        self.assertNotIn('sed "$DROP_LIB_TREE"', package)
        self.assertIn('sed "${DROP_LIB_TREE}s|', package)
        self.assertIn('cp "$ROOT/ATTRIBUTION.md" "$destination/"', (ROOT / "packaging/nightly-common.sh").read_text(),
                      "the tar and the macOS bundle require ATTRIBUTION.md")

    def test_the_appimage_apprun_is_a_script_only_for_nightly(self):
        package = (ROOT / "packaging/linux/package.sh").read_text()
        # Stable keeps Dylan's `ln -s usr/bin/filmcraft AppRun`: installing the script there would
        # write desktop entries and icons on every stable start, which is new stable behaviour.
        section = package[package.index('  APPDIR="$WORK/FilmCraft.AppDir"'):]
        nightly, stable = section.split("\n  else\n", 1)
        self.assertIn('install -Dm755 "$HERE/AppRun" "$APPDIR/AppRun"', nightly, "the Nightly installs the AppRun script")
        self.assertNotIn("ln -s usr/bin/filmcraft", nightly, "a Nightly AppRun must not be a symlink")
        self.assertNotIn('install -Dm755 "$HERE/AppRun"', stable, "stable installs no desktop entry or icon on start")
        self.assertIn('ln -s usr/bin/filmcraft "$APPDIR/AppRun"', stable)
        self.assertIn('$0 == "CHANNEL=stable" { print "CHANNEL=nightly-v1"; next }', nightly)
        apprun = ROOT / "packaging/linux/AppRun"
        self.assertTrue(apprun.is_file() and not apprun.is_symlink())
        self.assertTrue(apprun.stat().st_mode & 0o111, "AppRun must be executable")

    def test_the_plain_nightly_tar_ships_the_launcher_beside_the_gui(self):
        archive = (ROOT / "packaging/nightly-archive.py").read_text()
        self.assertIn('{"filmcraft", "filmcraft-bin", "filmcraft-nightly.png"}', archive)
        self.assertIn('"filmcraft", "filmcraft-bin", "filmcraft-nightly.png", "README.md"', archive)
        self.assertIn('"$STAGE/filmcraft-bin"', (ROOT / "packaging/linux/nightly.sh").read_text())
        check = (ROOT / "packaging/linux/nightly-check.sh").read_text()
        self.assertIn("for name in filmcraft filmcraft-bin filmcraft-nightly.png", check)

    def test_the_nightly_flatpak_install_keeps_the_launcher_and_drops_the_mime_tree(self):
        source = (ROOT / "packaging/linux/flatpak-bundle.sh").read_text()
        self.assertIn("/app/libexec/filmcraft", source)
        self.assertIn("/share\\/mime\\/packages\\//d", source)
        self.assertIn("--talk-name=org\\.freedesktop\\.Flatpak/d", source)



if __name__ == "__main__":
    unittest.main(verbosity=2)
