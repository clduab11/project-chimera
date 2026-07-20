## Summary

<!-- What does this PR change and why? Link issues with "Closes #N". -->

## Validation gate

All boxes must be checked before merge (see AGENTS.md `<validation_gate>`):

- [ ] `cargo test -p chimera-core` passes
- [ ] `forge test --root contracts/` passes (required for any `contracts/` change — AGENTS.md invariant #6)
- [ ] `python -m compileall scripts ai-audit/scripts` passes
- [ ] `slither contracts --config-file slither.config.json` reviewed (contract changes)
- [ ] `cargo fmt --all -- --check` and `cargo clippy -p chimera-core --all-targets -- -D warnings` clean

## Invariants

- [ ] `config/pacing.yaml` still matches `core/src/config.rs` defaults and the `valid_yaml()` fixture (invariant #1)
- [ ] `docs/snapshot-schema.md` still matches `ReserveData` in `core/src/simulator/prewarm.rs` (invariant #2)
- [ ] No `f64` for monetary values — `rust_decimal::Decimal` only (invariant #3)
- [ ] EVM changes target Cancun only (invariant #4)
- [ ] Python scripts still import without `web3.py` installed (invariant #5)
- [ ] No `execute_mode: live` committed in `config/` or `core/tests/` (shadow-guard)

## Safety

- [ ] No secrets, keystores, private keys, or funded addresses committed
- [ ] Guardrails (pacing caps, breakers, emergency pause) not weakened without an accompanying design doc
