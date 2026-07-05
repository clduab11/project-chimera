# T6 Follow-Up: Comment 1 & 2 — Complete Implementation Plan

## Comment 1: Fix EOA/Signer Mismatch (Worker-as-Executor Authorization)

**Problem**: `execute_live()` uses `opp.eoa` from the pacing engine EOA pool for nonce lookup, gas checks, and flash-loan receiver, but the runtime only has ONE configured private key. Nonce/gas reference a different address than the actual tx sender.

**Fix**: Pass the signer's address to the orchestrator. Pin live-mode EOA to the signer address. Shadow mode continues cycling the EOA pool for logging.

---

### Change 1a: `core/src/main.rs` — Lines 157-173

**Replace** lines 157-173 (`let signer = ...` through the end of the `match signer {}` block) with:

```rust
    // (c) Signer. Required for live; optional (read-only) for shadow. Never logs secrets.
    let signer = load_signer(&pacing_cfg.execute_mode)?;
    let signer_address: Option<Address> = signer.as_ref().map(|s| s.address());

    // (d) Provider-type divergence is resolved by monomorphizing `run_with_provider`
    //     over the two concrete provider types (wallet-backed vs read-only).
    //     The signer_address is forwarded so the orchestrator can pin live-mode
    //     nonce/gas/receiver to the actual signing wallet (multi-worker EOA rotation
    //     is deferred to T7 SignerRegistry).
    match signer {
        Some(signer) => {
            info!("Signer loaded; building wallet-backed provider");
            let wallet = EthereumWallet::from(signer);
            let provider = Arc::new(ProviderBuilder::new().wallet(wallet).connect_http(rpc_url));
            run_with_provider(provider, pacing_cfg, routing_cfg, risk_cfg, metrics, snapshot, snapshot_path, signer_address).await
        }
        None => {
            info!("No signer configured; building read-only provider (shadow)");
            let provider = Arc::new(ProviderBuilder::new().connect_http(rpc_url));
            run_with_provider(provider, pacing_cfg, routing_cfg, risk_cfg, metrics, snapshot, snapshot_path, signer_address).await
        }
    }
```

### Change 1b: `core/src/main.rs` — Function signature line 180-188

**Replace** the `run_with_provider` signature with:

```rust
async fn run_with_provider<P>(
    provider: Arc<P>,
    pacing_cfg: PacingConfig,
    routing_cfg: RoutingConfig,
    risk_cfg: RiskConfig,
    metrics: Arc<Metrics>,
    snapshot: MarketSnapshot,
    snapshot_path: PathBuf,
    signer_address: Option<Address>, // NEW
) -> anyhow::Result<()>
where
    P: Provider<Ethereum> + Clone + Send + Sync + 'static,
```

### Change 1c: `core/src/main.rs` — After line 257 (PacingEngine::new block)

**Insert** after `let pacing_engine = PacingEngine::new(...).with_eth_oracle(...);`:

```rust
    // If live and the EOA pool has entries that don't match the configured signer,
    // warn that multi-worker EOA rotation is deferred to T7. The orchestrator will
    // pin all live submissions to the signer address.
    if execute_mode == "live" {
        if let Some(ref sa) = signer_address {
            if let Some(pool_eoa) = pacing_engine.select_next_eoa() {
                if pool_eoa.parse::<Address>().map_or(true, |a| a != *sa) {
                    warn!(
                        target = "chimera::main",
                        signer = %sa,
                        pool_eoa = %pool_eoa,
                        "Live mode: EOA pool entry differs from configured signer. \
                         All live submissions will use the signer address. \
                         Multi-worker EOA rotation (T7) is deferred."
                    );
                }
            }
        }
    }
```

### Change 1d: `core/src/main.rs` — Line 268, Orchestrator::new()

**Add** `signer_address,` as the final argument in the `Orchestrator::new()` call.

---

### Change 1e: `core/src/orchestrator.rs` — Add signer_address field

**Find** the Orchestrator struct (around line 64-94). Add field after `risk_config`:
```rust
    signer_address: Option<Address>,
```

**Find** `Orchestrator::new()` (around line 96-130). Add parameter after `risk_cfg`:
```rust
    pub fn new(
        // ... existing params ...
        risk_cfg: RiskConfig,
        signer_address: Option<Address>,  // NEW
    ) -> Self {
```

**In the Self literal**, add line:
```rust
    signer_address,
```

### Change 1f: `core/src/orchestrator.rs` — Pin EOA in process_candidate()

**Locate** lines ~280-292 (`let eoa = self.pacing.engine().select_next_eoa()...` through `let opp = Opportunity { ... }`).

**Replace** with:
```rust
        // 3. Build the opportunity from REAL candidate + simulation fields + resolved route.
        // Live mode: EOA is pinned to the configured signer address (multi-worker
        // rotation deferred to T7 SignerRegistry). Shadow mode: cycle EOA pool normally.
        let eoa_raw = self.pacing.engine().select_next_eoa()
            .unwrap_or_else(|| FALLBACK_EOA.to_string());

        let eoa = if self.execute_mode == "live" {
            match self.signer_address {
                Some(signer_addr) => {
                    if eoa_raw.parse::<Address>().ok() != Some(signer_addr) {
                        warn!(
                            target = "chimera::orchestrator",
                            pool_eoa = %eoa_raw,
                            signer = %signer_addr,
                            "Pinning EOA to signer address for live submission (rotation deferred to T7)"
                        );
                    }
                    format!("0x{:x}", signer_addr)
                }
                None => {
                    return Err(ChimeraError::ConfigError(
                        "Live mode requires a configured signer address".into()
                    ));
                }
            }
        } else {
            eoa_raw
        };

        let opp = Opportunity {
            id: format!("liq-{}", candidate.user),
            expected_net_usd: sim_result.expected_profit_usd,
            gas_estimate_gwei,
            venue: route.venue_name.clone(),
            eoa,
            timestamp: chrono::Utc::now(),
        };
```

---

## Comment 2: Real min_profit and tip for Live Path

**Problem**: `execute_live()` hardcodes `min_profit = U256::ZERO` and `tip = U256::ZERO`, disabling the Executor.yul profit gate. The on-chain transaction has no minimum-profit enforcement.

**Fix**: Derive `min_profit` from `sim_result.expected_profit_usd` using the ETH/USD oracle price, scaled by a configurable `min_profit_fraction`. Add the fraction to `RiskConfig` and `risk.yaml`.

---

### Change 2a: `core/src/config.rs` — Add `min_profit_fraction` to RiskConfig

**Find** `RiskConfig` struct. **Add** after `min_profit`:
```rust
    /// Fraction of expected profit enforced as the on-chain `min_profit` gate.
    /// Range 0.0-1.0. Default 0.8 requires 80% of expected profit on-chain,
    /// protecting against sandwich attacks while leaving some buffer.
    #[serde(default = "default_min_profit_fraction")]
    pub min_profit_fraction: Decimal,
```

**Find** `default_min_profit` fn and **add** after it:
```rust
fn default_min_profit_fraction() -> Decimal {
    Decimal::from_str("0.8").expect("valid literal")
}
```

**In** `impl Default for RiskConfig`, **add** line:
```rust
    min_profit_fraction: Decimal::from_str("0.8").expect("valid literal"),
```

### Change 2b: `config/risk.yaml` — Add min_profit_fraction

**Add** a line after `min_profit: 2.5`:
```yaml
min_profit_fraction: 0.8
```

### Change 2c: `core/src/pacing_engine.rs` — Add cached_eth_price() accessor

**After** the `current_risk_state()` method (~line 572), **add**:
```rust
    /// Return the most recently cached ETH/USD price, if any.
    /// Returns the last successful oracle refresh value, or None if
    /// the oracle has never been refreshed.
    pub fn cached_eth_price(&self) -> Option<Decimal> {
        self.inner.read().cached_eth_price
    }
```

### Change 2d: `core/src/orchestrator.rs` — Replace zero min_profit/tip in execute_live()

**Replace** lines 440-441 (`let min_profit = U256::ZERO; let tip = U256::ZERO;`) with:

```rust
        // Derive min_profit from simulated economics and ETH/USD oracle.
        // The Executor.yul profit gate enforces:
        //   balanceAfter > balanceBefore + min_profit + tip
        // We set min_profit to a fraction of expected profit to protect against
        // sandwich attacks while leaving a buffer for gas/slippage variance.
        let eth_price = self.pacing.engine().cached_eth_price()
            .unwrap_or(self.pacing_cfg.eth_price_usd_fallback);

        let min_profit = if eth_price > Decimal::ZERO && sim_result.expected_profit_usd > Decimal::ZERO {
            // Convert expected_profit_usd to debt-asset wei (assumes WETH as debt).
            let profit_eth = sim_result.expected_profit_usd / eth_price;
            let wei_per_eth = Decimal::from(1_000_000_000_000_000_000u128);
            let profit_wei_dec = profit_eth * wei_per_eth;
            let profit_wei = profit_wei_dec
                .to_u128()
                .map(U256::from)
                .unwrap_or(U256::ZERO);

            // Scale by min_profit_fraction (e.g., 0.8 = require 80% expected profit)
            if profit_wei > U256::ZERO {
                let numerator = (self.risk_config.min_profit_fraction * Decimal::from(1000u64))
                    .to_u32()
                    .unwrap_or(800);
                profit_wei * U256::from(numerator) / U256::from(1000u64)
            } else {
                U256::ZERO
            }
        } else {
            U256::ZERO
        };

        // tip = 0 for now; configurable later via RiskConfig tip_bps
        let tip = U256::ZERO;
        let deadline = chrono::Utc::now().timestamp() as u64 + 300;
```

---

### Change 2e: `core/src/strategy/assembler.rs` — Extend tests

**Add** to the test module, after existing tests:

```rust
    #[test]
    fn test_build_transaction_live_min_profit_is_populated() {
        let aave_pool = address!("0xA238Dd80C259a72e81d7e4664a9801593F98d1Ab");
        let worker = address!("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE");
        let route = sample_route();
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000u64);
        let min_profit = U256::from(12345u64);
        let tip = U256::from(50u64);
        let deadline = 1700000000u64;

        let tx = StrategyAssembler::build_transaction(
            aave_pool, worker, &route,
            collateral, user, debt_to_cover, false,
            min_profit, tip, deadline, 1,
        );

        // Build expected params and verify min_profit at word 6 (offset 192)
        let params = StrategyAssembler::build_strategy_params_full(
            collateral, user, debt_to_cover, false,
            route.router, route.amount_out_min,
            min_profit, tip, deadline,
        );
        let encoded = params.encode();

        let mut mp_buf = [0u8; 32];
        mp_buf.copy_from_slice(&encoded[192..224]);
        assert_eq!(U256::from_be_bytes(mp_buf), min_profit,
            "word 6 (min_profit) should match live value");
        assert!(U256::from_be_bytes(mp_buf) > U256::ZERO,
            "live min_profit must be non-zero");
    }

    #[test]
    fn test_live_vs_shadow_min_profit_difference() {
        let route = sample_route();
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000u64);

        // Shadow: words 6-8 are zero
        let shadow_params = StrategyAssembler::build_strategy_params(
            collateral, user, debt_to_cover, false, route.router, route.amount_out_min,
        );
        let shadow_enc = shadow_params.encode();

        // Live: words 6-8 are populated
        let live_params = StrategyAssembler::build_strategy_params_full(
            collateral, user, debt_to_cover, false,
            route.router, route.amount_out_min,
            U256::from(100u64), U256::from(10u64), 2000000000u64,
        );
        let live_enc = live_params.encode();

        // Shadow fields at offsets 192-288 must differ from live
        let shadow_tail = &shadow_enc[192..288];
        let live_tail = &live_enc[192..288];
        assert!(shadow_tail.iter().all(|&b| b == 0),
            "shadow min_profit/tip/deadline must be zero");
        assert!(live_tail != shadow_tail,
            "live min_profit/tip/deadline must differ from shadow");
    }
```

---

## Summary of Changes

| # | File | Change |
|---|---|---|
| 1a | `core/src/main.rs:157-173` | Extract `signer_address`, pass to `run_with_provider()` |
| 1b | `core/src/main.rs:180` | Add `signer_address: Option<Address>` to fn signature |
| 1c | `core/src/main.rs:257` | Add EOA pool divergence warning in live mode |
| 1d | `core/src/main.rs:268` | Pass `signer_address` to `Orchestrator::new()` |
| 1e | `core/src/orchestrator.rs` | Add `signer_address` field + `Orchestrator::new()` param |
| 1f | `core/src/orchestrator.rs:280-292` | Pin EOA to signer address in live mode |
| 2a | `core/src/config.rs` | Add `min_profit_fraction: Decimal` to `RiskConfig` |
| 2b | `config/risk.yaml` | Add `min_profit_fraction: 0.8` |
| 2c | `core/src/pacing_engine.rs` | Add `cached_eth_price()` public accessor |
| 2d | `core/src/orchestrator.rs:440-441` | Compute real `min_profit` from ETH/USD price + fraction |
| 2e | `core/src/strategy/assembler.rs` | Add 2 tests: non-zero min_profit, live vs shadow |
