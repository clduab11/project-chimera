//! EOA balance verification for gas pre-flight checks.
//! Called before transaction submission to prevent insufficient-funds reverts.

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use tracing::warn;

use crate::ChimeraError;

/// Minimum gas budget for a liquidation transaction on L2.
/// Conservative: 250k gas × 50 gwei = 0.0125 ETH buffer.
pub const MIN_GAS_BUDGET_WEI: u128 = 12_500_000_000_000_000; // 0.0125 ETH

/// Check whether an EOA has enough native balance to cover gas.
///
/// Returns `Ok(true)` if the wallet has at least `MIN_GAS_BUDGET_WEI`
/// more than the estimated gas cost. Returns `Ok(false)` if underfunded.
pub async fn check_eoa_gas_sufficient<P: Provider + Sync>(
    provider: &P,
    eoa: Address,
    estimated_gas_cost_wei: U256,
) -> Result<bool, ChimeraError> {
    let balance = provider
        .get_balance(eoa)
        .await
        .map_err(|e| ChimeraError::RpcError(format!("get_balance failed for {eoa}: {e}")))?;

    let required = estimated_gas_cost_wei.saturating_add(U256::from(MIN_GAS_BUDGET_WEI));

    if balance < required {
        warn!(
            target = "chimera::balance",
            %eoa,
            balance_wei = %balance,
            required_wei = %required,
            shortfall_wei = %(required - balance),
            "EOA has insufficient gas balance — skipping opportunity"
        );
        return Ok(false);
    }
    Ok(true)
}

/// Return the native balance of an address as a U256.
pub async fn get_eoa_balance<P: Provider + Sync>(
    provider: &P,
    eoa: Address,
) -> Result<U256, ChimeraError> {
    provider
        .get_balance(eoa)
        .await
        .map_err(|e| ChimeraError::RpcError(format!("get_balance failed for {eoa}: {e}")))
}
