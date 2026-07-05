# Subagent Testing Routines

Claude Code sessions in this repo have a set of committed subagent routines
that run the full test surface in parallel and triage failures. They live in
`.claude/agents/` and are orchestrated by `.claude/workflows/test-suite.js`.

## The routines

| Routine | Lane | What it runs |
|---------|------|--------------|
| `.claude/agents/rust-test-runner.md` | rust | `cargo fmt --check` (advisory — repo has no rustfmt contract), `cargo clippy -p chimera-core --all-targets -- -D warnings`, `cargo test -p chimera-core` (unit + integration suites in `core/tests/` + proptest + golden replays) |
| `.claude/agents/python-test-runner.md` | python | `python3 -m compileall scripts ai-audit/scripts`, `python3 -m pytest tests/ -v` |
| `.claude/agents/contract-test-runner.md` | contracts | `forge build` + `forge test --root contracts/ -vvv`; reports SKIPPED with workstation instructions when Foundry is absent |
| `.claude/agents/guardrail-checker.md` | guardrails | shadow-guard grep (no committed `execute_mode: live`), pacing-cap sanity vs `pacing_canonical.yaml`, snapshot-schema sync with `core/src/simulator/prewarm.rs`, no-secrets scan |
| `.claude/agents/test-triage.md` | (post-hoc) | one instance per failure: classifies product-bug / test-bug / env-issue / flaky, locates root cause, flags funds-path relevance; diagnoses only, never edits |

## Running the suite

Ask a Claude Code session to run the `test-suite` workflow, or invoke it
directly. All four lanes run in parallel; each lane's failures go to triage
as soon as that lane finishes (fast lanes triage while the Rust lane is
still compiling).

Run a subset by passing a lane list as workflow args, e.g. `["rust",
"python"]`.

## Design rules baked into the routines

- **Observe-only**: lane runners and triage never edit source, tests,
  fixtures, or config. Fixes are a deliberate follow-up step, not a side
  effect of testing.
- **Run everything**: a lane does not stop at the first failure; the report
  is always complete.
- **No live-chain contact**: no broadcast, no `forge script`, no production
  RPC URLs. Tests are offline-by-design; a test that reaches for the
  network is reported as a finding.
- **Graceful degradation**: a missing toolchain (forge, slither) produces a
  SKIPPED entry with the exact workstation commands, never a silent pass —
  consistent with the validation-gate warning in the README.
- **Single source of truth**: the workflow script points each lane agent at
  its `.claude/agents/*.md` file, so editing a routine does not require
  touching the orchestration.

## Relationship to CI

The lanes mirror `.github/workflows/ci.yml` (rust, forge, python,
shadow-guard) and add pytest (CI currently only runs `compileall`) plus the
pacing/schema/no-secrets guardrails. Dependency-audit jobs (cargo-audit,
pip-audit, osv-scanner, slither) remain CI-only because their tools are not
guaranteed in an agent sandbox; the contract runner's SKIPPED reporting
covers the gap explicitly.
