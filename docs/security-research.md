# Security Research Compendium

Project Chimera depends on protocol correctness, compiler correctness, and conservative operational controls. This page is the canonical security-research index for the repository.

## Primary Protocol Sources

- Aave V3 documentation: https://aave.com/docs/aave-v3/overview
- Aave V3 smart-contract docs: https://aave.com/docs/aave-v3/smart-contracts
- Aave V3 Origin source: https://github.com/aave-dao/aave-v3-origin
- Aave address book: https://github.com/bgd-labs/aave-address-book
- DeepWiki liquidation logic reference: https://deepwiki.com/aave/aave-v3-core/4.2-liquidation-logic

Use Aave V3 Origin and the Aave address book as canonical. The older `aave/aave-v3-core` repository is useful for historical source links and DeepWiki pages, but it is marked deprecated upstream.

## Context7 Findings Incorporated

- Alloy RPC contract bindings require `#[sol(rpc)]` on `sol!` interfaces that create contract instances and call builders.
- Foundry tests should inherit `forge-std/Test.sol`; external calls can be tested with `vm.mockCall`, `vm.expectCall`, and `vm.expectRevert`.
- Slither supports JSON and SARIF output, `--filter-paths`, detector inclusion/exclusion, and config-file-driven runs.

## Compiler and Toolchain Advisory Checks

- Solidity known bugs: https://docs.soliditylang.org/en/latest/bugs.html
- Solidity `bugs.json`: https://github.com/argotorg/solidity/blob/develop/docs/bugs.json
- Solidity `bugs_by_version.json`: https://github.com/argotorg/solidity/blob/develop/docs/bugs_by_version.json
- GitHub Advisory Database: https://github.com/advisories
- RustSec advisory database: https://github.com/RustSec/advisory-db
- OSV database: https://osv.dev/

Before live mode, verify the exact `solc_version`, `via_ir`, optimizer settings, and EVM version against Solidity compiler known bugs. Pure Yul and inline assembly bypass many Solidity safety checks, so all arithmetic, memory, returndata, and selector handling needs manual review.

## MCP/Searchable Security Compendiums

- Trail of Bits Slither MCP: https://github.com/trailofbits/slither-mcp
- Slither static analyzer: https://github.com/crytic/slither
- Trail of Bits publications: https://github.com/trailofbits/publications

The audit pipeline should treat Slither MCP/Slither outputs as structured evidence, not final verdicts. Automated tools are a first pass; manually confirm every high-severity or economic finding.

## DeFi-Specific Risk Scenarios

Track these as regression cases in golden replays and simulation tests:

- Oracle staleness and price-source divergence.
- CAPO or internal safety-mechanism misconfiguration.
- eMode collateral/debt overrides.
- Isolation-mode debt ceiling and liquidation restrictions.
- Non-18-decimal collateral/debt math.
- L2 sequencer stalls and L1 data-fee spikes.
- ERC20 nonstandard return behavior.
- DEX slippage, path manipulation, and stale amount-out assumptions.

## Required Local Security Commands

```bash
cargo audit
cargo deny check
pip-audit
osv-scanner -r .
slither contracts --config-file slither.config.json --sarif ai-audit/queue/slither.sarif
```

These commands are optional in a fresh clone until the tools are installed, but they are required before any controlled live execution milestone.
