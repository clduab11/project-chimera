//! Aave price oracle integration.
//!
//! Implements [`PriceOracle`] using the Aave `getAssetPrice` view function.
//!
//! On canonical Aave V3 deployments the price oracle's base currency is USD with
//! 8 decimals (`BASE_CURRENCY_UNIT = 1e8`), so prices are normalised from 8 decimals
//! to [`Decimal`]. This keeps Aave and Chainlink (also 8-decimal USD) consistent.
//!
//! This oracle serves as a fallback when Chainlink feeds are unavailable
//! or when the Aave price is required for consistency.

use crate::{ChimeraError, OraclePrice, PriceOracle};
use alloy::network::Ethereum;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Solidity interface
// ---------------------------------------------------------------------------

alloy::sol! {
    /// Aave price oracle interface.
    #[sol(rpc)]
    interface IAavePriceOracle {
        function getAssetPrice(address asset) external view returns (uint256);
    }
}

// ---------------------------------------------------------------------------
// AaveOracle
// ---------------------------------------------------------------------------

/// On-chain Aave price oracle client.
///
/// `P` is an Alloy provider (e.g. `alloy::providers::HttpProvider`).
#[derive(Debug, Clone)]
pub struct AaveOracle<P: Provider<Ethereum> + Clone> {
    provider: P,
    oracle_address: Address,
    staleness: Duration,
}

impl<P: Provider<Ethereum> + Clone> AaveOracle<P> {
    /// Create a new Aave oracle client.
    ///
    /// # Arguments
    /// * `provider` — Alloy RPC provider.
    /// * `oracle_address` — The Aave `PriceOracle` contract address.
    /// * `staleness` — maximum acceptable age for a price update (used by the
    ///   [`PriceOracle::is_stale`] check; Aave itself does not expose `updatedAt`).
    pub fn new(provider: P, oracle_address: Address, staleness: Duration) -> Self {
        Self {
            provider,
            oracle_address,
            staleness,
        }
    }

    /// Fetch the raw [`OraclePrice`] for an asset.
    ///
    /// # Note
    /// Aave's `getAssetPrice` does not return a timestamp. The `timestamp`
    /// field in the returned [`OraclePrice`] is set to the current system time,
    /// so [`PriceOracle::is_stale`] should be used with care. For stricter
    /// guarantees, prefer [`ChainlinkOracle`] which exposes `updatedAt`.
    ///
    /// # Errors
    /// * [`ChimeraError::OracleError`] — if the RPC call fails.
    pub async fn get_oracle_price(&self, asset: Address) -> Result<OraclePrice, ChimeraError> {
        let contract = IAavePriceOracle::new(self.oracle_address, &self.provider);
        let result = contract
            .getAssetPrice(asset)
            .call()
            .await
            .map_err(|e| ChimeraError::OracleError(format!("Aave oracle call failed: {e}")))?;

        // Aave V3 oracle returns prices in its base currency (USD) with 8 decimals.
        // Modern alloy returns the single return value directly (no `_0` wrapper).
        let price = u256_to_decimal(result, 8)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(OraclePrice {
            price,
            timestamp: now,
            source: format!("aave:{}", self.oracle_address),
        })
    }
}

#[async_trait]
impl<P: Provider<Ethereum> + Clone + Send + Sync> PriceOracle for AaveOracle<P> {
    async fn get_price(&self, asset: Address) -> Result<Decimal, ChimeraError> {
        self.get_oracle_price(asset).await.map(|op| op.price)
    }

    fn staleness_threshold(&self) -> Duration {
        self.staleness
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a [`U256`] value with `decimals` fractional digits into a [`Decimal`].
fn u256_to_decimal(value: U256, decimals: u32) -> Result<Decimal, ChimeraError> {
    let int_part: i128 = value
        .try_into()
        .map_err(|_| ChimeraError::ConversionError(format!("U256 {value} exceeds i128 range")))?;
    Decimal::try_from_i128_with_scale(int_part, decimals)
        .map_err(|e| ChimeraError::ConversionError(format!("Decimal conversion failed: {e}")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn u256_to_decimal_happy_path() {
        // 1500 ETH with 18 decimals => 1500.0
        let value = U256::from(1_500_u64) * U256::from(10).pow(U256::from(18));
        let d = u256_to_decimal(value, 18).unwrap();
        assert_eq!(d, dec!(1500));
    }

    #[test]
    fn u256_to_decimal_fractional() {
        // 0.5 ETH = 5e17 wei
        let value = U256::from(5_u64) * U256::from(10).pow(U256::from(17));
        let d = u256_to_decimal(value, 18).unwrap();
        assert_eq!(d, dec!(0.5));
    }

    #[test]
    fn u256_to_decimal_zero() {
        let d = u256_to_decimal(U256::ZERO, 18).unwrap();
        assert_eq!(d, Decimal::ZERO);
    }

    #[test]
    fn oracle_price_source_format() {
        let op = OraclePrice {
            price: dec!(2000.00),
            timestamp: 1_700_000_000,
            source: "aave:0xA50BA011c48153Ed2469D2dA71d8C35A1eB5A38f".into(),
        };
        assert!(op.source.starts_with("aave:"));
    }
}
