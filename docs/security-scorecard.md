# Repository security scorecard

[OpenSSF Scorecard](https://github.com/ossf/scorecard-action) scans upstream `main`
every Monday at 06:23 UTC and when its workflow changes. Results go to the
[Scorecard viewer](https://scorecard.dev/viewer/?uri=github.com/storytold/filmcraft),
GitHub code scanning, and a `scorecard-sarif` Actions artifact retained for seven days.
The first published result needs a successful run after upstream merge.

The workflow runs only on `storytold/filmcraft` and `main`, with pinned actions,
checkout credentials disabled, and job-scoped repository-read, security-upload and
OIDC permissions. It uses the default GitHub token and no PAT. Fork and pull-request
code cannot run this publishing job.

Repository rules are readable with the default token. Classic branch-protection
settings may not be, so an incomplete check does not prove protection is absent.
Review the individual findings and workflow logs before interpreting a score.
Scorecard assesses repository practices; it does not replace application testing,
code review or vulnerability reporting.
