# Release Readiness — v0.2.0 First Release Plan

**Date**: 2026-07-20 · **Status**: in progress · **Target**: tag `v0.2.0`

This document is the comprehensive readiness assessment for the project's first
release. It records what was verified, what changed, and what remains an
operator action. Scope: repository release hygiene only — **not** live-mode
authorization (live remains gated by the 7-day soak and go-live runbooks).

---

## 1. Validation gate status

| Gate (AGENTS.md) | Result | Where |
|---|---|---|
| `cargo test -p chimera-core` | ✅ **199 passed, 0 failed, 3 ignored** | this machine, 2026-07-20 |
| `cargo clippy -p chimera-core --all-targets -- -D warnings` | ✅ clean | this machine, 2026-07-20 |
| `cargo fmt --all -- --check` | ✅ clean | this machine, 2026-07-20 |
| `python -m compileall scripts ai-audit/scripts` | ✅ exit 0 | this machine, 2026-07-20 |
| `forge test --root contracts/` | ⏳ **pending** — forge not installed here | workstation |
| `slither contracts --config-file slither.config.json` | ⏳ **pending** — slither not installed here | workstation |

## 2. Gap register and resolution

| # | Gap | Severity | Resolution |
|---|-----|----------|------------|
| 1 | LICENSE (MIT) contradicted README "Private — not for public distribution" | **blocking** | ✅ LICENSE replaced with proprietary notice; README aligned; crate `license = "Proprietary"` |
| 2 | Crate version 0.1.0 vs CHANGELOG 0.1.4; placeholder `repository` URL in `core/Cargo.toml` | **blocking** | ✅ bumped to 0.2.0; URL set to `github.com/clduab11/project-chimera` |
| 3 | README doc index linked missing `PHASE6_VALIDATION_GATE.md` (revoked per Wave 0 notes); index missing newer docs | high | ✅ index corrected |
| 4 | CI: duplicated "Restore Yul sources" step in slither job; no fmt/clippy gate | high | ✅ deduplicated; `lint` job added (`fmt --check` + `clippy -D warnings`); ci.yml made reusable via `workflow_call` |
| 5 | No release automation; no git tags existed | high | ✅ `.github/workflows/release.yml`: tag==version check → full ci.yml reused as the gate (no drift) → build → publish bundle; token read-by-default, write scoped to publish; actions SHA-pinned |
| 6 | 13 rustc warnings + 8 clippy findings | medium | ✅ all fixed; `-D warnings` now enforced. Post-review: one import removal (`CrashRecovery`) was valid only on Windows — restored with matching `#[cfg(not(target_os = "windows"))]` gating after the review caught the Linux build break |
| 7 | `commit.txt` (scratch notes) tracked in git | medium | ✅ untracked (`git rm --cached`), gitignored |
| 8 | Untracked conversation exports (`chimera-expansion-*.md`) risked accidental publication (II documents a refused fraud request) | medium | ✅ added to `.gitignore` (files stay local) |
| 9 | No dependabot, PR template, or issue templates | medium | ✅ added |
| 10 | No documented monetization/compliance posture | high | ✅ `docs/monetization.md` (Paths A/B/C, rejected approaches, compliance checklist, staged gates) |
| 11 | `rustup-init.exe`, `temp_executor.yul`, `.pytest_cache/`, `target/`, `contracts/out/` present on disk | low | already gitignored — no action; optionally clean local disk |
| 12 | No CODEOWNERS / code-of-conduct | low | deferred — single-maintainer private repo |

## 3. Secrets & artifact hygiene (verified 2026-07-20)

- No tracked `.key`/`.pem`/keystore/seed files; `config/eoa_pool.json` holds
  placeholder addresses with zeroed balances and an explicit "never fund" notice.
- `config/testnet.base-sepolia.json` contains public testnet constants only.
- Foundry `out/`、`cache/`, logs, `core/state/`, `ai-audit/queue/` are gitignored.

## 4. Remaining operator actions to cut v0.2.0

1. Review the working-tree diff (see `git status`), then commit:
   `git add -A && git commit -m "chore(release): prepare v0.2.0 — proprietary license, lint gates, release workflow, readiness + monetization docs"`.
2. On a full-toolchain machine: `forge test --root contracts/` and
   `slither contracts --config-file slither.config.json` (CI also runs both).
3. Push, confirm CI green (including new `lint` job).
4. Tag and release: `git tag v0.2.0 && git push origin v0.2.0` — the release
   workflow verifies tag↔version match, reruns tests, builds the Linux binary,
   and publishes a private GitHub release with the bundle.
5. Delete local-only scratch if desired: `rustup-init.exe`, `temp_executor.yul`.

## 5. Explicitly out of scope for this release

- Live-mode execution (gated by 7-day soak + keystore/multisig runbooks).
- Protected/private transaction submission (tracked as pre-live requirement).
- Any paid service, financial transaction, or account creation — see
  `docs/monetization.md` for the staged, approval-gated plan.
- The rejected approaches enumerated in `docs/monetization.md` §5.

## 6. Post-release operational path (unchanged project gates)

testnet practice → 7-day shadow soak → compliance review
(`docs/monetization.md` §6) → explicit go-live decision → capped live
operation → weekly PnL/breaker review.
