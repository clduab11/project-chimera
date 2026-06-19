//! Chainlink price oracle integration.
//!
//! Implements [`PriceOracle`] using Chainlink AggregatorV3Interface feeds.
//! Each asset must be mapped to its corresponding Chainlink feed address.

use crate::{ChimeraError, OraclePrice, PriceOracle};
use alloy::network::Ethereum;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Solidity interface
// ---------------------------------------------------------------------------

alloy::sol! {
    /// Chainlink AggregatorV3Interface.
    #[sol(rpc)]
    interface AggregatorV3Interface {
        function latestRoundData() external view returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
    }
}

// ---------------------------------------------------------------------------
// ChainlinkOracle
// ---------------------------------------------------------------------------

/// On-chain Chainlink price oracle client.
///
/// `P` is an Alloy provider (e.g. `alloy::providers::HttpProvider`).
#[derive(Debug, Clone)]
pub struct ChainlinkOracle<P: Provider<Ethereum> + Clone> {
    provider: P,
    /// Mapping from asset address → Chainlink feed address.
    feeds: HashMap<Address, Address>,
    staleness: Duration,
}

impl<P: Provider<Ethereum> + Clone> ChainlinkOracle<P> {
    /// Create a new Chainlink oracle client.
    ///
    /// # Arguments
    /// * `provider` — Alloy RPC provider.
    /// * `feeds` — asset → feed address mapping.
    /// * `staleness` — maximum acceptable age for a price update.
    pub fn new(provider: P, feeds: HashMap<Address, Address>, staleness: Duration) -> Self {
        Self {
            provider,
            feeds,
            staleness,
        }
    }

    /// Add or overwrite a feed mapping for an asset.
    pub fn register_feed(&mut self, asset: Address, feed: Address) {
        self.feeds.insert(asset, feed);
    }

    /// Fetch the raw [`OraclePrice`] for an asset, including timestamp and source metadata.
    ///
    /// # Errors
    /// * [`ChimeraError::OracleError`] — if the feed is not registered or the price is stale.
    pub async fn get_oracle_price(&self, asset: Address) -> Result<OraclePrice, ChimeraError> {
        let feed_address =
            self.feeds.get(&asset).copied().ok_or_else(|| {
                ChimeraError::OracleError(format!("no Chainlink feed for {asset}"))
            })?;

        let contract = AggregatorV3Interface::new(feed_address, &self.provider);
        let result = contract
            .latestRoundData()
            .call()
            .await
            .map_err(|e| ChimeraError::OracleError(format!("Chainlink call failed: {e}")))?;

        // `updatedAt` is a uint256 on-chain; normalise to u64 seconds for staleness checks.
        let updated_at: u64 = result.updatedAt.try_into().map_err(|_| {
            ChimeraError::OracleError(format!(
                "Chainlink updatedAt for {asset} exceeds u64 range ({})",
                result.updatedAt
            ))
        })?;
        if self.is_stale(updated_at) {
            return Err(ChimeraError::OracleError(format!(
                "Chainlink price for {asset} is stale (updatedAt={updated_at})"
            )));
        }

        // Chainlink answers are signed integers (can be negative in theory).
        let answer = result.answer;
        if answer.is_negative() {
            return Err(ChimeraError::OracleError(format!(
                "Chainlink price for {asset} is negative ({answer})"
            )));
        }

        // Convert the (non-negative) int256 answer to Decimal via its raw U256 representation.
        // Most Chainlink feeds use 8 decimals, but we normalise explicitly.
        let answer_u256 = answer.into_raw();
        let price = u256_to_decimal(answer_u256, 8)?;

        Ok(OraclePrice {
            price,
            timestamp: updated_at,
            source: format!("chainlink:{feed_address}"),
        })
    }
}

#[async_trait]
impl<P: Provider<Ethereum> + Clone + Send + Sync> PriceOracle for ChainlinkOracle<P> {
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
    // U256 → i128 is safe for all realistic price values (even ETH/USD ≈ 2e21 fits easily).
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
        // 123456789 with 8 decimals => 1.23456789
        let value = U256::from(123_456_789_u64);
        let d = u256_to_decimal(value, 8).unwrap();
        assert_eq!(d, dec!(1.23456789));
    }

    #[test]
    fn u256_to_decimal_zero() {
        let d = u256_to_decimal(U256::ZERO, 8).unwrap();
        assert_eq!(d, Decimal::ZERO);
    }

    #[test]
    fn u256_to_decimal_large_value() {
        // ETH/USD ≈ 2000 * 1e8 = 2e11 — still trivial for i128.
        let value = U256::from(200_000_000_000_u64);
        let d = u256_to_decimal(value, 8).unwrap();
        assert_eq!(d, dec!(2000));
    }

    #[test]
    fn oracle_price_metadata() {
        let op = OraclePrice {
            price: dec!(1500.00),
            timestamp: 1_700_000_000,
            source: "chainlink:0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419".into(),
        };
        assert!(op.source.starts_with("chainlink:"));
    }
}
