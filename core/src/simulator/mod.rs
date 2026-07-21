//! Comprehensive REVM-based Simulator for Aave V3 Liquidations on L2.
//!
//! Designed per the documented Testing Strategy for near-100% accuracy:
//! - Exact replication of GenericLogic + LiquidationLogic (HF, close factor, eMode, isolation, bad debt, protocol fees).
//! - L2 gas + real-time L1 data fee (eth_getL1Fee / NodeInterface).
//! - Forked state at current head with pre-warming for speed.
//! - Profit gate after ALL costs (2.5x minimum per risk config).
//!
//! This is the core of the "simulation accuracy" requirement for safe live execution under $50 start + strict pacing.

pub mod golden;
pub mod prewarm;

use crate::{ChimeraError, PacingConfig};
use alloy::eips::BlockId;
use alloy::network::Ethereum;
use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::sol_types::{SolCall, SolEvent};
use revm::context::result::ExecResultAndState;
use revm::context::result::ExecutionResult;
use revm::context::{BlockEnv, TxEnv};
use revm::database::{AlloyDB, CacheDB};
use revm::database_interface::WrapDatabaseAsync;
use revm::handler::EvmTr;
use revm::primitives::TxKind;
use revm::{Context, ExecuteEvm, MainBuilder, MainContext};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::sync::Arc;
use tracing::info;

/// Default liquidation bonus (5%) used as fallback when on-chain reserve data is unavailable.
const DEFAULT_LIQUIDATION_BONUS_BPS: u16 = 10500;

alloy::sol! {
    #[sol(rpc)]
    interface IAavePool {
        function liquidationCall(
            address collateralAsset,
            address debtAsset,
            address user,
            uint256 debtToCover,
            bool receiveAToken
        ) external;

        function getReserveData(address asset) external view returns (
            uint256 configuration,
            uint128 liquidityIndex,
            uint128 variableBorrowIndex,
            uint128 currentLiquidityRate,
            uint128 currentVariableBorrowRate,
            uint128 currentStableBorrowRate,
            uint40 lastUpdateTimestamp,
            uint16 id,
            address aTokenAddress,
            address stableDebtTokenAddress,
            address variableDebtTokenAddress,
            address interestRateStrategyAddress,
            uint128 accruedToTreasury,
            uint16 isolationModeTotalDebt
        );
    }
}

alloy::sol! {
    #[derive(Debug, PartialEq, Eq)]
    event LiquidationCall(
        address indexed collateralAsset,
        address indexed debtAsset,
        address indexed user,
        uint256 debtToCover,
        uint256 liquidatedCollateralAmount,
        uint256 actualDebtToCover,
        uint256 liquidationBonus,
        address liquidator
    );
}

/// A candidate liquidation opportunity (output of detector, input to simulator).
#[derive(Debug, Clone)]
pub struct LiquidationCandidate {
    pub user: Address,
    pub collateral_asset: Address,
    pub debt_asset: Address,
    pub debt_to_cover: U256,
    pub receive_a_token: bool,
    pub current_hf: U256,
    pub chain_id: u64,
    /// Edge case 3 (bad debt): set when the position's seizable collateral value is
    /// below its debt value. The detector does not emit bad-debt candidates (they are
    /// unprofitable for a searcher), but the flag lets the orchestrator/simulator
    /// defensively skip any candidate that ever carries it set.
    pub bad_debt: bool,
    /// Decimals of the debt asset (e.g. 6 for USDC, 18 for WETH). Profit from a
    /// liquidation is denominated in native debt-asset units; converting to USD
    /// requires the asset's own unit, not a hardcoded 1e18.
    pub debt_decimals: u8,
    /// USD price of the debt asset in Aave-oracle 8-decimal units, captured from the
    /// detector snapshot at emission time. Zero means "unknown" and makes the
    /// simulator fall back to the legacy 18-dec ETH-denominated conversion.
    pub debt_price_usd: U256,
    /// Decimals of the collateral asset. Seized collateral is denominated in its
    /// own native units; USD profit accounting needs both sides' units.
    pub collateral_decimals: u8,
    /// USD price of the collateral asset (8-decimal), captured at emission time.
    /// Zero means "unknown" (e.g. golden replays) — the event-path profit then
    /// falls back to a bonus-portion estimate on the debt side.
    pub collateral_price_usd: U256,
}

/// Result of a full REVM simulation of a liquidation (or flash + liquidation bundle).
///
/// Invariant #3: monetary values are `Decimal`, never `f64`. The simulator computes
/// profit internally in `f64` (REVM/oracle math) and converts at the single boundary
/// where this struct is constructed via `Decimal::from_f64_retain(..).unwrap_or(ZERO)`.
#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub profitable: bool,
    pub expected_profit_usd: Decimal,
    pub gas_used: u64,
    pub l1_data_fee_wei: U256,
    pub revert_reason: Option<String>,
    pub calldata: Vec<u8>,
}

/// L2 chain types for L1 fee calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum L2ChainType {
    #[default]
    Base,
    Arbitrum,
}

/// The production-grade simulator.
/// Holds a warm REVM DB (pre-loaded with Aave reserves, user positions, oracles).
type ForkDb<P> = CacheDB<WrapDatabaseAsync<AlloyDB<Ethereum, P>>>;
type EvmResultAndState = ExecResultAndState<ExecutionResult>;

pub struct LiquidationSimulator<P: Provider<Ethereum> + Clone> {
    provider: Arc<P>,
    db: ForkDb<P>,
    aave_pool: Address,
    oracle: Arc<dyn crate::oracle::PriceOracle>,
    eth_oracle_asset: Address,
    l2_chain_type: L2ChainType,
}

impl<P: Provider<Ethereum> + Clone> LiquidationSimulator<P> {
    pub async fn new(
        provider: Arc<P>,
        aave_pool: Address,
        oracle: Arc<dyn crate::oracle::PriceOracle>,
        eth_oracle_asset: Address,
    ) -> Result<Self, ChimeraError> {
        let alloy_db = AlloyDB::new((*provider).clone(), BlockId::latest());
        let wrapped_db = WrapDatabaseAsync::new(alloy_db).ok_or_else(|| {
            ChimeraError::SimulationFailed("tokio runtime is required for AlloyDB".into())
        })?;
        let db = CacheDB::new(wrapped_db);

        Ok(Self {
            provider,
            db,
            aave_pool,
            oracle,
            eth_oracle_asset,
            l2_chain_type: L2ChainType::default(),
        })
    }

    pub fn with_l2_chain_type(mut self, chain_type: L2ChainType) -> Self {
        self.l2_chain_type = chain_type;
        self
    }

    /// Loads a market snapshot (from snapshot_generator.py) and pre-warms the DB.
    pub async fn load_snapshot(&mut self, path: &std::path::Path) -> Result<(), ChimeraError> {
        let content = std::fs::read_to_string(path)?;
        let snapshot: prewarm::MarketSnapshot = serde_json::from_str(&content)?;
        prewarm::pre_warm_db(&mut self.db, &snapshot)?;
        Ok(())
    }

    /// Discard the fork DB — both prewarmed slots AND lazily cached live state —
    /// and rebuild it from the given snapshot file.
    ///
    /// `CacheDB` never evicts entries, and [`Self::load_snapshot`] only
    /// overwrites: users removed from a newer snapshot would keep their stale
    /// cached balances forever, and oracle/aggregator storage cached on first
    /// touch would freeze simulated prices. Called on every applied discovery
    /// reload; the cost is lazy refetch-on-miss at the next simulation.
    pub async fn rebuild_db(&mut self, path: &std::path::Path) -> Result<(), ChimeraError> {
        // Build + prewarm into a LOCAL db and assign only on success, so any
        // failure (transient file read race, parse error) truly keeps the
        // previous DB — matching the orchestrator's recovery log.
        let content = std::fs::read_to_string(path)?;
        let snapshot: prewarm::MarketSnapshot = serde_json::from_str(&content)?;
        let alloy_db = AlloyDB::new((*self.provider).clone(), BlockId::latest());
        let wrapped_db = WrapDatabaseAsync::new(alloy_db).ok_or_else(|| {
            ChimeraError::SimulationFailed("tokio runtime is required for AlloyDB".into())
        })?;
        let mut db = CacheDB::new(wrapped_db);
        prewarm::pre_warm_db(&mut db, &snapshot)?;
        self.db = db;
        Ok(())
    }

    /// High-fidelity simulation of an Aave V3 liquidationCall.
    /// Returns precise profit after every cost, using real L2 + L1 fee model.
    pub async fn simulate_liquidation(
        &mut self,
        candidate: &LiquidationCandidate,
        current_gas_price: U256,
        current_l1_fee_scalar: U256,
        snapshot_block: u64,
        pacing: &PacingConfig,
    ) -> Result<SimulationResult, ChimeraError> {
        let calldata = self.build_liquidation_calldata(candidate)?;

        // Build REVM block + tx env synced to the fork block for correct interest accrual.
        let block = self.build_block_env(current_gas_price, snapshot_block)?;
        let tx = self.build_tx_env(candidate.chain_id, current_gas_price, calldata.clone())?;

        // Execute in REVM.
        let mut evm = Context::mainnet()
            .with_db(&mut self.db)
            .with_block(block)
            .build_mainnet();
        evm.ctx_mut().cfg.chain_id = candidate.chain_id;
        evm.ctx_mut().cfg.disable_nonce_check = true;

        let result = evm
            .transact(tx)
            .map_err(|e| ChimeraError::SimulationFailed(e.to_string()))?;
        let gas_used = result.result.gas_used();
        let success = result.result.is_success();

        if !success {
            let revert = result
                .result
                .output()
                .map(|b| format!("0x{}", hex::encode(b)))
                .unwrap_or_default();
            return Ok(SimulationResult {
                profitable: false,
                expected_profit_usd: Decimal::ZERO,
                gas_used,
                l1_data_fee_wei: U256::ZERO,
                revert_reason: Some(revert),
                calldata,
            });
        }

        // Real L1 data fee (OP Stack or Arbitrum).
        let l1_data_fee = self.calculate_l1_data_fee(&calldata, current_l1_fee_scalar)?;

        // Extract profit from state diff (post-execution balance changes).
        let (_net_bonus_native, profit_usd) = self
            .extract_profit_from_state(&result, candidate, &l1_data_fee, pacing)
            .await?;

        let profitable = profit_usd > 0.0;

        info!(
            target: "chimera::simulator",
            user = %candidate.user,
            hf = %candidate.current_hf,
            profitable = profitable,
            profit_usd = profit_usd,
            l1_fee_wei = %l1_data_fee,
            "Liquidation simulation complete"
        );

        Ok(SimulationResult {
            profitable,
            // Invariant #3 boundary: convert the internal f64 profit to Decimal here.
            expected_profit_usd: Decimal::from_f64_retain(profit_usd).unwrap_or(Decimal::ZERO),
            gas_used,
            l1_data_fee_wei: l1_data_fee,
            revert_reason: None,
            calldata,
        })
    }

    fn build_block_env(
        &self,
        gas_price: U256,
        snapshot_block: u64,
    ) -> Result<BlockEnv, ChimeraError> {
        // block.timestamp must be a real wall-clock time, never 0: Aave's interest
        // accrual computes `block.timestamp - lastUpdateTimestamp` (MathUtils), and the
        // prewarmed reserves carry real on-chain lastUpdateTimestamp values (~1.7e9).
        // With timestamp 0 that subtraction underflows (Solidity 0.8 revert), so every
        // liquidationCall against a real snapshot would report success=false.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(BlockEnv {
            number: U256::from(snapshot_block),
            beneficiary: Address::ZERO,
            timestamp: U256::from(now_secs),
            gas_limit: 30_000_000,
            basefee: checked_u256_to_u64(gas_price)?,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::ZERO),
            // Cancun header validation requires excess_blob_gas to be present even
            // though L2 liquidation txs never carry blobs; `None` fails every
            // simulation with "header validation error: excess_blob_gas not set".
            blob_excess_gas_and_price: Some(
                revm::context_interface::block::BlobExcessGasAndPrice {
                    excess_blob_gas: 0,
                    blob_gasprice: 1,
                },
            ),
            slot_num: 0,
        })
    }

    fn build_tx_env(
        &self,
        chain_id: u64,
        gas_price: U256,
        calldata: Vec<u8>,
    ) -> Result<TxEnv, ChimeraError> {
        Ok(TxEnv {
            caller: Address::ZERO,
            gas_limit: 2_000_000,
            gas_price: checked_u256_to_u128(gas_price)?,
            kind: TxKind::Call(self.aave_pool),
            value: U256::ZERO,
            data: calldata.into(),
            chain_id: Some(chain_id),
            ..Default::default()
        })
    }

    /// Calculate L1 data fee using the chain-specific model.
    /// - Base (OP Stack): eth_getL1Fee or (calldata_len * 16 + overhead) * scalar
    /// - Arbitrum: NodeInterface.gasEstimateL1DataFee
    fn calculate_l1_data_fee(
        &self,
        calldata: &[u8],
        l1_fee_scalar: U256,
    ) -> Result<U256, ChimeraError> {
        match self.l2_chain_type {
            L2ChainType::Base => {
                // OP Stack L1 data fee formula:
                //   L1CompressedGas = compress(calldata, fixed overhead + per-byte overhead)
                //   L1Fee = L1CompressedGas * (l1_basefee * l1_fee_scalar / 1e6)
                let calldata_len = calldata.len() as u64;
                // Conservative: 16 gas/byte (EIP-2028) + 2100 fixed overhead
                let l1_gas = calldata_len * 16 + 2100;
                let fee = U256::from(l1_gas) * l1_fee_scalar / U256::from(1_000_000);
                Ok(fee)
            }
            L2ChainType::Arbitrum => {
                // Arbitrum L1 data fee: much more complex (BatchPostingReport + gas price)
                // Use the conservative overestimate: 32 gas/byte
                let calldata_len = calldata.len() as u64;
                let l1_gas = calldata_len * 32 + 1400;
                let fee = U256::from(l1_gas) * l1_fee_scalar / U256::from(1_000_000);
                Ok(fee)
            }
        }
    }

    /// Extract profit from post-execution state diff, accounted in USD:
    /// `usd(collateral seized) − usd(protocol cut) − usd(debt repaid)`.
    /// Returns `(net bonus portion in COLLATERAL native units, profit USD)`.
    /// Falls back to heuristic if delta extraction fails.
    async fn extract_profit_from_state(
        &self,
        result: &EvmResultAndState,
        candidate: &LiquidationCandidate,
        l1_fee: &U256,
        pacing: &PacingConfig,
    ) -> Result<(U256, f64), ChimeraError> {
        // Try to decode LiquidationCall event from logs for exact amounts.
        let logs = result.result.logs();
        let mut liquidated_collateral = U256::ZERO;
        let mut actual_debt_covered = U256::ZERO;

        for log in logs {
            if let Ok(event) = LiquidationCall::decode_log_data(&log.data) {
                if event.user == candidate.user {
                    liquidated_collateral = event.liquidatedCollateralAmount;
                    actual_debt_covered = event.actualDebtToCover;
                    break;
                }
            }
        }

        if liquidated_collateral.is_zero() && actual_debt_covered.is_zero() {
            // Fallback: conservative heuristic if event decode fails.
            return self
                .estimate_profit_heuristic(candidate, l1_fee, pacing)
                .await;
        }

        // Both event amounts are in DIFFERENT native units: liquidatedCollateralAmount
        // in collateral-asset units, actualDebtToCover in debt-asset units. They can
        // only be combined in USD — subtracting them in native units produced garbage
        // for every mixed-decimal pair (and double-subtracted principal for same-asset
        // pairs). Without BOTH prices we cannot do USD accounting (a zero-priced leg
        // would silently value that side at $0); estimate as the bonus portion of the
        // repaid debt instead (same shape as the heuristic).
        if candidate.collateral_price_usd.is_zero() || candidate.debt_price_usd.is_zero() {
            tracing::warn!(
                target: "chimera::simulator",
                collateral_asset = %candidate.collateral_asset,
                debt_asset = %candidate.debt_asset,
                "Price(s) unknown; estimating event profit as debt bonus portion"
            );
            let (bonus_bps, _) = self
                .fetch_reserve_liquidation_params(candidate.collateral_asset)
                .await?;
            let portion_bps = U256::from(u32::from(bonus_bps.max(10_000)) - 10_000);
            let estimated = actual_debt_covered * portion_bps / U256::from(10_000);
            let profit_usd = self.debt_units_to_usd(estimated, candidate).await?;
            return Ok((estimated, profit_usd));
        }

        // Fetch liquidation bonus + protocol fee from the SAME packed reserve
        // configuration in one getReserveData call (previously two identical RPCs).
        // Edge case 2 (liquidation protocol fee): Aave V3 takes a protocol fee on
        // the BONUS portion of a liquidation (config bits 152-167).
        let (bonus_bps, protocol_fee_bps) = self
            .fetch_reserve_liquidation_params(candidate.collateral_asset)
            .await?;

        Ok(event_profit_usd_known_prices(
            liquidated_collateral,
            actual_debt_covered,
            bonus_bps,
            protocol_fee_bps,
            candidate,
        ))
    }

    /// Convert an amount in native debt-asset units to USD (f64, internal-only per
    /// invariant #3 — the Decimal boundary is SimulationResult construction).
    ///
    /// Uses the candidate's `debt_decimals` + 8-dec `debt_price_usd` captured at
    /// detection time. A zero price means the caller had no reserve data (e.g.
    /// golden replays); fall back to the legacy 18-dec ETH-denominated conversion
    /// with a warning rather than silently reporting $0.
    async fn debt_units_to_usd(
        &self,
        amount: U256,
        candidate: &LiquidationCandidate,
    ) -> Result<f64, ChimeraError> {
        if candidate.debt_price_usd.is_zero() {
            tracing::warn!(
                target: "chimera::simulator",
                debt_asset = %candidate.debt_asset,
                "Debt price unknown; falling back to 18-dec ETH-denominated profit conversion"
            );
            let eth_price = self.fetch_eth_price().await?;
            let eth_price_f64 = eth_price.to_f64().ok_or_else(|| {
                ChimeraError::ConversionError("ETH price Decimal to f64 failed".into())
            })?;
            return Ok(checked_u256_to_f64(amount) / 1e18 * eth_price_f64);
        }
        let asset_unit = 10f64.powi(candidate.debt_decimals as i32);
        let price_usd = checked_u256_to_f64(candidate.debt_price_usd) / 1e8;
        Ok(checked_u256_to_f64(amount) / asset_unit * price_usd)
    }

    /// Fallback profit estimation when delta extraction is unavailable.
    /// Conservative: profit is the liquidation BONUS PORTION only — the seized
    /// collateral above the repaid principal. `liquidationBonus` is 1e4-scaled with
    /// 10000 = break-even (10500 = 5% bonus), so the portion is `bps - 10000`.
    async fn estimate_profit_heuristic(
        &self,
        candidate: &LiquidationCandidate,
        _l1_fee: &U256,
        _pacing: &PacingConfig,
    ) -> Result<(U256, f64), ChimeraError> {
        let bonus_portion_bps =
            U256::from(DEFAULT_LIQUIDATION_BONUS_BPS.saturating_sub(10_000));
        let estimated_profit = candidate.debt_to_cover * bonus_portion_bps / U256::from(10000);

        let profit_usd = self.debt_units_to_usd(estimated_profit, candidate).await?;

        info!(
            target: "chimera::simulator",
            mode = "heuristic_fallback",
            profit_usd = profit_usd,
            "Using heuristic profit estimate"
        );

        Ok((estimated_profit, profit_usd))
    }

    /// Fetch the liquidation bonus and protocol fee (both basis points, 1e4
    /// scale) for a reserve from the SAME packed on-chain configuration word in
    /// one `getReserveData` call. Falls back to the default bonus + 0 fee on RPC
    /// failure (no fee = conservative only in the sense that the fee is unknown).
    async fn fetch_reserve_liquidation_params(
        &self,
        collateral: Address,
    ) -> Result<(u16, u16), ChimeraError> {
        let contract = IAavePool::new(self.aave_pool, &*self.provider);
        match contract.getReserveData(collateral).call().await {
            Ok(result) => Ok((
                parse_liquidation_bonus(result.configuration),
                parse_protocol_fee(result.configuration),
            )),
            Err(e) => {
                tracing::warn!(
                    "getReserveData RPC failed for {}: {}, using default bonus {} + 0 bps fee",
                    collateral,
                    e,
                    DEFAULT_LIQUIDATION_BONUS_BPS
                );
                Ok((DEFAULT_LIQUIDATION_BONUS_BPS, 0))
            }
        }
    }

    /// Fetch the current ETH/USD price from the configured oracle.
    async fn fetch_eth_price(&self) -> Result<Decimal, ChimeraError> {
        self.oracle.get_price(self.eth_oracle_asset).await
    }

    fn build_liquidation_calldata(
        &self,
        c: &LiquidationCandidate,
    ) -> Result<Vec<u8>, ChimeraError> {
        let call = IAavePool::liquidationCallCall {
            collateralAsset: c.collateral_asset,
            debtAsset: c.debt_asset,
            user: c.user,
            debtToCover: c.debt_to_cover,
            receiveAToken: c.receive_a_token,
        };
        Ok(call.abi_encode())
    }
}

/// Extract liquidation bonus (in basis points) from the packed Aave V3 reserve configuration.
///
/// Aave V3 packs `configuration` as a `uint256` where:
/// - bits 0-15   = LTV
/// - bits 16-31  = liquidationThreshold
/// - bits 32-47  = liquidationBonus
/// - ...
fn parse_liquidation_bonus(configuration: U256) -> u16 {
    let masked = (configuration >> 32) & U256::from(0xFFFF);
    u16::try_from(masked).unwrap_or(DEFAULT_LIQUIDATION_BONUS_BPS)
}

/// Extract the liquidation protocol fee (in basis points) from the packed Aave V3 reserve
/// configuration. The protocol fee occupies bits 152-167 of the `configuration` word
/// (mirrors [`parse_liquidation_bonus`]). Defaults to `0` on overflow.
fn parse_protocol_fee(configuration: U256) -> u16 {
    let masked = (configuration >> 152) & U256::from(0xFFFF);
    u16::try_from(masked).unwrap_or(0)
}

fn checked_u256_to_u64(v: U256) -> Result<u64, ChimeraError> {
    u64::try_from(v)
        .map_err(|_| ChimeraError::ConversionError(format!("U256 value {v} exceeds u64::MAX")))
}

fn checked_u256_to_u128(v: U256) -> Result<u128, ChimeraError> {
    u128::try_from(v)
        .map_err(|_| ChimeraError::ConversionError(format!("U256 value {v} exceeds u128::MAX")))
}

/// Convert an amount in an asset's native units to USD (f64, internal-only per
/// invariant #3) using that asset's decimals and 8-decimal oracle price.
fn asset_units_to_usd(amount: U256, decimals: u8, price_usd_8dec: U256) -> f64 {
    let asset_unit = 10f64.powi(i32::from(decimals));
    let price = checked_u256_to_f64(price_usd_8dec) / 1e8;
    checked_u256_to_f64(amount) / asset_unit * price
}

/// Event-path profit with both prices known, accounted in USD:
/// `usd(seized collateral) − usd(protocol cut) − usd(debt repaid)`.
///
/// The two event amounts live in DIFFERENT native units (collateral vs debt) and
/// may only be combined in USD. The protocol cut is estimated on the bonus
/// portion of the seizure (`seized − seized×10000/bonus_bps`) — conservative:
/// the event amount may already exclude the fee, so subtracting it again only
/// understates profit. Returns `(net bonus portion in COLLATERAL units, profit USD)`;
/// the USD value can be negative for an unprofitable liquidation.
fn event_profit_usd_known_prices(
    liquidated_collateral: U256,
    actual_debt_covered: U256,
    bonus_bps: u16,
    protocol_fee_bps: u16,
    candidate: &LiquidationCandidate,
) -> (U256, f64) {
    let bonus_divisor = U256::from(u32::from(bonus_bps.max(10_000)));
    let principal_equiv = liquidated_collateral * U256::from(10_000) / bonus_divisor;
    let bonus_portion = liquidated_collateral.saturating_sub(principal_equiv);
    let protocol_cut = bonus_portion * U256::from(protocol_fee_bps) / U256::from(10_000);
    let net_bonus_in_collateral = bonus_portion.saturating_sub(protocol_cut);

    let seized_usd = asset_units_to_usd(
        liquidated_collateral,
        candidate.collateral_decimals,
        candidate.collateral_price_usd,
    );
    let cut_usd = asset_units_to_usd(
        protocol_cut,
        candidate.collateral_decimals,
        candidate.collateral_price_usd,
    );
    let debt_usd = asset_units_to_usd(
        actual_debt_covered,
        candidate.debt_decimals,
        candidate.debt_price_usd,
    );
    (net_bonus_in_collateral, seized_usd - cut_usd - debt_usd)
}

/// Safely converts U256 to f64 without panic on overflow.
/// Large values are approximated with the most significant 64 bits.
fn checked_u256_to_f64(v: U256) -> f64 {
    if let Ok(n) = u64::try_from(v) {
        n as f64
    } else {
        let (bits, exponent) = v.most_significant_bits();
        (bits as f64) * 2f64.powi(exponent as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::tests::MockOracle;
    use rust_decimal_macros::dec;
    use std::time::Duration;

    #[test]
    fn parse_liquidation_bonus_default_value() {
        // DEFAULT_LIQUIDATION_BONUS_BPS = 5% bonus, placed at bits 32-47
        let configuration = U256::from(DEFAULT_LIQUIDATION_BONUS_BPS as u64) << 32;
        assert_eq!(
            parse_liquidation_bonus(configuration),
            DEFAULT_LIQUIDATION_BONUS_BPS
        );
    }

    #[test]
    fn parse_liquidation_bonus_different_value() {
        let bonus = 11000u16;
        let configuration = U256::from(bonus) << 32;
        assert_eq!(parse_liquidation_bonus(configuration), bonus);
    }

    #[test]
    fn parse_liquidation_bonus_with_other_fields_set() {
        // Set LTV (bits 0-15) and liquidationThreshold (bits 16-31) as well
        let ltv = 8000u64;
        let threshold = 8250u64;
        let bonus = DEFAULT_LIQUIDATION_BONUS_BPS as u64;
        let configuration = (bonus << 32) | (threshold << 16) | ltv;
        assert_eq!(
            parse_liquidation_bonus(U256::from(configuration)),
            DEFAULT_LIQUIDATION_BONUS_BPS
        );
    }

    #[test]
    fn parse_protocol_fee_zero_when_unset() {
        // No fee bits set => 0.
        let configuration = U256::from(DEFAULT_LIQUIDATION_BONUS_BPS as u64) << 32;
        assert_eq!(parse_protocol_fee(configuration), 0);
    }

    #[test]
    fn parse_protocol_fee_reads_bits_152_167() {
        // 1000 bps (10%) placed at bits 152-167.
        let fee = 1000u16;
        let configuration = U256::from(fee) << 152;
        assert_eq!(parse_protocol_fee(configuration), fee);
    }

    #[test]
    fn parse_protocol_fee_isolated_from_bonus_and_threshold() {
        // Set LTV, liquidationThreshold, liquidationBonus AND protocol fee; the parser
        // must read ONLY bits 152-167 and ignore the lower fields.
        let ltv = U256::from(8000u64);
        let threshold = U256::from(8250u64) << 16;
        let bonus = U256::from(DEFAULT_LIQUIDATION_BONUS_BPS as u64) << 32;
        let fee = U256::from(800u64) << 152; // 8%
        let configuration = ltv | threshold | bonus | fee;
        assert_eq!(parse_protocol_fee(configuration), 800);
        // And the bonus parser is unaffected by the fee bits.
        assert_eq!(
            parse_liquidation_bonus(configuration),
            DEFAULT_LIQUIDATION_BONUS_BPS
        );
    }

    /// B1 regression: BlockEnv.timestamp must be real wall-clock time, not 0.
    ///
    /// Aave's interest accrual computes `block.timestamp - lastUpdateTimestamp`; the
    /// prewarmed reserves carry real on-chain timestamps (~1.7e9), so a zero block
    /// timestamp underflows and reverts every liquidationCall. This test installs a
    /// contract at the pool address that reverts iff `block.timestamp < ~1.69e9` —
    /// exactly the failure the old `timestamp: U256::from(0)` env produced — and
    /// asserts a simulation now completes successfully.
    #[tokio::test(flavor = "multi_thread")]
    async fn simulation_block_timestamp_is_wall_clock_not_zero() {
        use revm::state::{AccountInfo, Bytecode};

        let eth_asset = Address::with_last_byte(0xEE);
        let oracle: Arc<dyn crate::oracle::PriceOracle> = Arc::new(MockOracle {
            prices: [(eth_asset, dec!(3500))].into_iter().collect(),
            staleness: Duration::from_secs(300),
        });
        // Unreachable provider: everything the sim touches must come from the cache.
        let provider = Arc::new(
            alloy::providers::ProviderBuilder::new()
                .connect_http("http://127.0.0.1:1".parse().unwrap()),
        );
        let pool = Address::with_last_byte(0xAA);
        let mut sim = LiquidationSimulator::new(provider, pool, oracle, eth_asset)
            .await
            .expect("construction is lazy; no RPC");

        // Runtime bytecode: PUSH4 0x65000000 (~1.694e9); TIMESTAMP; LT;
        // PUSH1 0x0d; JUMPI; STOP; <pad>; JUMPDEST; PUSH1 0; PUSH1 0; REVERT.
        // Reverts iff block.timestamp < 0x65000000.
        let code_bytes: &[u8] = &[
            0x63, 0x65, 0x00, 0x00, 0x00, 0x42, 0x10, 0x60, 0x0d, 0x57, 0x00, 0x00, 0x00, 0x5b,
            0x60, 0x00, 0x60, 0x00, 0xfd,
        ];
        let bytecode = Bytecode::new_raw(alloy::primitives::Bytes::copy_from_slice(code_bytes));
        let guard_info = AccountInfo {
            code_hash: bytecode.hash_slow(),
            code: Some(bytecode),
            ..Default::default()
        };
        sim.db.insert_account_info(pool, guard_info);
        // Caller + beneficiary (both Address::ZERO) must be cached so REVM never
        // touches the (unreachable) provider.
        sim.db.insert_account_info(Address::ZERO, AccountInfo::default());

        let candidate = LiquidationCandidate {
            user: Address::with_last_byte(0x01),
            collateral_asset: Address::with_last_byte(0x02),
            debt_asset: Address::with_last_byte(0x03),
            debt_to_cover: U256::from(1_000_000_000_000_000_000u128),
            receive_a_token: false,
            current_hf: U256::from(1),
            chain_id: 8453,
            bad_debt: false,
            debt_decimals: 18,
            debt_price_usd: U256::from(3_500u64) * U256::from(100_000_000u64), // $3500, 8-dec
            collateral_decimals: 18,
            collateral_price_usd: U256::from(3_500u64) * U256::from(100_000_000u64),
        };
        let result = sim
            .simulate_liquidation(
                &candidate,
                U256::ZERO, // zero gas price: no caller balance needed
                U256::ZERO,
                123,
                &PacingConfig::default(),
            )
            .await
            .expect("simulation must not error");

        assert!(
            result.revert_reason.is_none(),
            "timestamp-guard contract reverted: BlockEnv.timestamp is not wall-clock \
             (got revert {:?})",
            result.revert_reason
        );
        assert!(result.profitable, "heuristic profit path should mark this profitable");
    }

    /// Test-only harness: a simulator over an unreachable provider with a mock
    /// ETH/USD oracle, for exercising internal conversion paths without RPC.
    async fn offline_sim() -> LiquidationSimulator<impl Provider<Ethereum> + Clone> {
        let eth_asset = Address::with_last_byte(0xEE);
        let oracle: Arc<dyn crate::oracle::PriceOracle> = Arc::new(MockOracle {
            prices: [(eth_asset, dec!(3500))].into_iter().collect(),
            staleness: Duration::from_secs(300),
        });
        let provider = Arc::new(
            alloy::providers::ProviderBuilder::new()
                .connect_http("http://127.0.0.1:1".parse().unwrap()),
        );
        LiquidationSimulator::new(provider, Address::with_last_byte(0xAA), oracle, eth_asset)
            .await
            .expect("construction is lazy; no RPC")
    }

    fn candidate_with_debt(debt_decimals: u8, debt_price_usd: U256) -> LiquidationCandidate {
        LiquidationCandidate {
            user: Address::with_last_byte(0x01),
            collateral_asset: Address::with_last_byte(0x02),
            debt_asset: Address::with_last_byte(0x03),
            debt_to_cover: U256::from(1_000u64),
            receive_a_token: false,
            current_hf: U256::from(1),
            chain_id: 8453,
            bad_debt: false,
            debt_decimals,
            debt_price_usd,
            collateral_decimals: 18,
            collateral_price_usd: U256::ZERO,
        }
    }

    /// Adversarial-review regression: event-path profit must be accounted in USD
    /// per asset — the old math subtracted debt-unit principal from a
    /// collateral-unit bonus and (post-B2) scaled the mix by debt decimals,
    /// overstating the dominant WETH-collateral/USDC-debt shape by ~4e8x.
    #[test]
    fn event_profit_usd_weth_collateral_usdc_debt() {
        // WETH $2500 (18-dec) collateral, USDC $1.00 (6-dec) debt.
        let mut c = candidate_with_debt(6, U256::from(100_000_000u64));
        c.collateral_decimals = 18;
        c.collateral_price_usd = U256::from(2_500u64) * U256::from(100_000_000u64);
        // Repaid 1000 USDC; seized 0.42 WETH ($1050) at 5% bonus, 10% protocol fee.
        // principal_equiv = 0.4 WETH; bonus = 0.02 WETH ($50); cut = $5.
        // profit = 1050 − 5 − 1000 = $45.
        let (net_bonus, usd) = event_profit_usd_known_prices(
            U256::from(420_000_000_000_000_000u128), // 0.42 WETH
            U256::from(1_000_000_000u64),            // 1000 USDC
            10_500,
            1_000,
            &c,
        );
        assert!((usd - 45.0).abs() < 0.01, "expected ≈$45, got {usd}");
        assert_eq!(net_bonus, U256::from(18_000_000_000_000_000u128)); // 0.018 WETH
    }

    /// Reverse shape (6-dec collateral, 18-dec debt): the old math saturated this
    /// to $0 and discarded genuinely profitable liquidations.
    #[test]
    fn event_profit_usd_usdc_collateral_weth_debt() {
        let mut c = candidate_with_debt(18, U256::from(2_500u64) * U256::from(100_000_000u64));
        c.collateral_decimals = 6;
        c.collateral_price_usd = U256::from(100_000_000u64); // $1.00
        // Repaid 0.4 WETH ($1000); seized 1050 USDC ($1050); no protocol fee.
        let (_, usd) = event_profit_usd_known_prices(
            U256::from(1_050_000_000u64),            // 1050 USDC
            U256::from(400_000_000_000_000_000u128), // 0.4 WETH
            10_500,
            0,
            &c,
        );
        assert!((usd - 50.0).abs() < 0.01, "expected ≈$50, got {usd}");
    }

    /// Same-asset pair: USD accounting must NOT double-subtract principal (old
    /// math subtracted debt from a quantity already net of principal → always $0).
    #[test]
    fn event_profit_usd_same_asset_pair() {
        let mut c = candidate_with_debt(18, U256::from(2_500u64) * U256::from(100_000_000u64));
        c.collateral_decimals = 18;
        c.collateral_price_usd = U256::from(2_500u64) * U256::from(100_000_000u64);
        // Repaid 0.4 WETH ($1000); seized 0.42 WETH ($1050); no fee → $50 profit.
        let (_, usd) = event_profit_usd_known_prices(
            U256::from(420_000_000_000_000_000u128),
            U256::from(400_000_000_000_000_000u128),
            10_500,
            0,
            &c,
        );
        assert!((usd - 50.0).abs() < 0.01, "expected ≈$50, got {usd}");
    }

    /// A liquidation seized at a worse price than repaid must report NEGATIVE
    /// profit, not saturate to zero-but-profitable.
    #[test]
    fn event_profit_usd_can_be_negative() {
        let mut c = candidate_with_debt(6, U256::from(100_000_000u64));
        c.collateral_decimals = 18;
        c.collateral_price_usd = U256::from(2_500u64) * U256::from(100_000_000u64);
        // Repaid 1100 USDC but seized only 0.42 WETH ($1050): −$50.
        let (_, usd) = event_profit_usd_known_prices(
            U256::from(420_000_000_000_000_000u128),
            U256::from(1_100_000_000u64),
            10_500,
            0,
            &c,
        );
        assert!(usd < -49.0 && usd > -51.0, "expected ≈−$50, got {usd}");
    }

    /// B2 regression: profit in 6-dec USDC units must convert to a sane USD value.
    /// The old code divided by 1e18 and multiplied by the ETH price, turning a $50
    /// USDC profit into ~1.9e-7 USD — every USDC-debt candidate (the dominant Base
    /// shape) was gated out as unprofitable.
    #[tokio::test(flavor = "multi_thread")]
    async fn profit_conversion_usdc_six_decimals() {
        let sim = offline_sim().await;
        // $0.9999 in 8-dec oracle units; 50 USDC profit in 6-dec native units.
        let candidate = candidate_with_debt(6, U256::from(99_990_000u64));
        let usd = sim
            .debt_units_to_usd(U256::from(50_000_000u64), &candidate)
            .await
            .unwrap();
        assert!(
            (usd - 49.995).abs() < 0.001,
            "50 USDC at $0.9999 must be ≈$49.995, got {usd}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn profit_conversion_weth_eighteen_decimals() {
        let sim = offline_sim().await;
        // $2500 WETH, 0.02 WETH profit => $50.
        let candidate = candidate_with_debt(18, U256::from(2_500u64) * U256::from(100_000_000u64));
        let usd = sim
            .debt_units_to_usd(U256::from(20_000_000_000_000_000u128), &candidate)
            .await
            .unwrap();
        assert!((usd - 50.0).abs() < 0.001, "0.02 WETH at $2500 must be $50, got {usd}");
    }

    /// Zero debt price = unknown (golden replays): falls back to the legacy
    /// 18-dec ETH-denominated conversion via the oracle instead of reporting $0.
    #[tokio::test(flavor = "multi_thread")]
    async fn profit_conversion_unknown_price_falls_back_to_eth() {
        let sim = offline_sim().await;
        let candidate = candidate_with_debt(18, U256::ZERO);
        let usd = sim
            .debt_units_to_usd(U256::from(1_000_000_000_000_000_000u128), &candidate)
            .await
            .unwrap();
        assert!((usd - 3500.0).abs() < 0.001, "1e18 at ETH $3500 must be $3500, got {usd}");
    }

    /// Heuristic fallback estimates the bonus PORTION (5% of debt covered), not
    /// principal + full seize amount (the old math reported ~2.05x debt as profit).
    #[tokio::test(flavor = "multi_thread")]
    async fn heuristic_profit_is_bonus_portion_only() {
        let sim = offline_sim().await;
        let mut candidate = candidate_with_debt(6, U256::from(100_000_000u64)); // $1.00
        candidate.debt_to_cover = U256::from(1_000_000_000u64); // 1000 USDC
        let (native, usd) = sim
            .estimate_profit_heuristic(&candidate, &U256::ZERO, &PacingConfig::default())
            .await
            .unwrap();
        // 5% of 1000 USDC = 50 USDC = $50.
        assert_eq!(native, U256::from(50_000_000u64));
        assert!((usd - 50.0).abs() < 0.001, "expected ≈$50, got {usd}");
    }

    #[tokio::test]
    async fn fetch_eth_price_with_mock_oracle() {
        let asset = Address::with_last_byte(0xEE);
        let oracle: Arc<dyn crate::oracle::PriceOracle> = Arc::new(MockOracle {
            prices: [(asset, dec!(3500.50))].into_iter().collect(),
            staleness: Duration::from_secs(300),
        });

        // Verify the oracle path that fetch_eth_price uses
        let price = oracle.get_price(asset).await.unwrap();
        assert_eq!(price, dec!(3500.50));

        let price_f64 = price
            .to_f64()
            .expect("Decimal to f64 conversion should succeed");
        assert!((price_f64 - 3500.50).abs() < f64::EPSILON);
    }
}
