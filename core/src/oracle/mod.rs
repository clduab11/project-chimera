//! Price oracle integration for Project Chimera.
//!
//! Provides unified access to on-chain price feeds via Chainlink and Aave oracles.
//! All prices are returned as [`Decimal`] for precise financial calculations.

use alloy::primitives::Address;
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::Duration;

use crate::ChimeraError;

pub mod aave;
pub mod chainlink;

pub use aave::AaveOracle;
pub use chainlink::ChainlinkOracle;

/// A captured price observation from an on-chain oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OraclePrice {
    /// The normalized price value.
    pub price: Decimal,
    /// Unix timestamp (seconds) when the price was last updated on-chain.
    pub timestamp: u64,
    /// Human-readable identifier for the source oracle (e.g. "chainlink:0x1234…").
    pub source: String,
}

/// Unified interface for on-chain price oracles.
///
/// Implementations must be [`Send`] + [`Sync`] so they can be shared across
/// async tasks (e.g. the pacing engine and the simulator).
#[async_trait]
pub trait PriceOracle: Send + Sync {
    /// Fetch the current price for a single asset.
    ///
    /// # Errors
    /// Returns [`ChimeraError::OracleError`] if the feed is stale, the asset
    /// is not supported, or the RPC call fails.
    async fn get_price(&self, asset: Address) -> Result<Decimal, ChimeraError>;

    /// Batch-fetch prices for multiple assets.
    ///
    /// The default implementation calls [`get_price`] for each asset in series.
    /// Implementations that support batched RPC calls (e.g. multicall) should
    /// override this for efficiency.
    ///
    /// # Errors
    /// Returns [`ChimeraError::OracleError`] if any individual fetch fails.
    async fn get_prices(
        &self,
        assets: Vec<Address>,
    ) -> Result<HashMap<Address, Decimal>, ChimeraError> {
        let mut map = HashMap::with_capacity(assets.len());
        for asset in assets {
            let price = self.get_price(asset).await?;
            map.insert(asset, price);
        }
        Ok(map)
    }

    /// Maximum age a price is allowed to have before it is considered stale.
    fn staleness_threshold(&self) -> Duration;

    /// Check whether a price timestamp is older than the configured staleness threshold.
    ///
    /// Uses the current system time as the reference; callers may override
    /// this behaviour by comparing against a specific block timestamp.
    fn is_stale(&self, timestamp: u64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(timestamp) > self.staleness_threshold().as_secs()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use async_trait::async_trait;
    use rust_decimal_macros::dec;

    /// A mock oracle useful for unit tests that don't need real RPC.
    pub struct MockOracle {
        pub prices: HashMap<Address, Decimal>,
        pub staleness: Duration,
    }

    #[async_trait]
    impl PriceOracle for MockOracle {
        async fn get_price(&self, asset: Address) -> Result<Decimal, ChimeraError> {
            self.prices
                .get(&asset)
                .copied()
                .ok_or_else(|| ChimeraError::OracleError(format!("unknown asset {asset}")))
        }

        fn staleness_threshold(&self) -> Duration {
            self.staleness
        }
    }

    #[test]
    fn oracle_price_struct_roundtrip() {
        let op = OraclePrice {
            price: dec!(1234.5678),
            timestamp: 1_700_000_000,
            source: "test".into(),
        };
        assert_eq!(op.price, dec!(1234.5678));
        assert_eq!(op.timestamp, 1_700_000_000);
        assert_eq!(op.source, "test");
    }

    #[tokio::test]
    async fn mock_oracle_get_price_happy_path() {
        let asset = Address::with_last_byte(0x42);
        let oracle = MockOracle {
            prices: [(asset, dec!(999.99))].into_iter().collect(),
            staleness: Duration::from_secs(300),
        };
        let price = oracle.get_price(asset).await.unwrap();
        assert_eq!(price, dec!(999.99));
    }

    #[tokio::test]
    async fn mock_oracle_get_price_unknown_asset() {
        let oracle = MockOracle {
            prices: HashMap::new(),
            staleness: Duration::from_secs(300),
        };
        let asset = Address::with_last_byte(0xAB);
        let err = oracle.get_price(asset).await.unwrap_err();
        assert!(matches!(err, ChimeraError::OracleError(_)));
    }

    #[tokio::test]
    async fn mock_oracle_get_prices_batch() {
        let a1 = Address::with_last_byte(0x01);
        let a2 = Address::with_last_byte(0x02);
        let oracle = MockOracle {
            prices: [(a1, dec!(1.0)), (a2, dec!(2.0))].into_iter().collect(),
            staleness: Duration::from_secs(300),
        };
        let prices = oracle.get_prices(vec![a1, a2]).await.unwrap();
        assert_eq!(prices.len(), 2);
        assert_eq!(prices[&a1], dec!(1.0));
        assert_eq!(prices[&a2], dec!(2.0));
    }

    #[test]
    fn is_stale_with_future_timestamp() {
        let oracle = MockOracle {
            prices: HashMap::new(),
            staleness: Duration::from_secs(300),
        };
        // Timestamp in the future should never be stale
        let future = u64::MAX;
        assert!(!oracle.is_stale(future));
    }
}
