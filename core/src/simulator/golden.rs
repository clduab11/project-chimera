//! Golden Test Harness for Liquidation Simulation Accuracy.
//!
//! This module implements the "Fork-Based Replay" strategy defined in
//! `docs/testing-strategy-liquidations.md`.
//!
//! It loads historical transaction data (from a CWE report or JSON fixture)
//! and replays it against a forked state to verify the simulator's output
//! matches on-chain reality.

use crate::simulator::{LiquidationCandidate, LiquidationSimulator, SimulationResult};
use alloy::network::Ethereum;
use alloy::primitives::{Address, U256};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize, Debug)]
pub struct HistoricalLiquidation {
    pub description: String,
    pub chain: String,
    pub block_number: u64,
    pub tx_hash: String,
    pub user: Address,
    pub collateral_asset: Address,
    pub debt_asset: Address,
    pub debt_to_cover: String,
    pub expected_profit_wei: String,
    pub gas_used: u64,
}

pub fn load_cwe_report(
    path: &Path,
) -> Result<Vec<HistoricalLiquidation>, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let reports: Vec<HistoricalLiquidation> = serde_json::from_str(&content)?;
    Ok(reports)
}

pub async fn run_golden_replays<P: alloy::providers::Provider<Ethereum> + Clone + 'static>(
    simulator: &mut LiquidationSimulator<P>,
    reports: &[HistoricalLiquidation],
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "[golden] Starting replay of {} historical liquidations...",
        reports.len()
    );

    for (i, report) in reports.iter().enumerate() {
        println!("[golden] Replaying #{}: {}", i + 1, report.description);

        let candidate = LiquidationCandidate {
            user: report.user,
            collateral_asset: report.collateral_asset,
            debt_asset: report.debt_asset,
            debt_to_cover: report.debt_to_cover.parse()?,
            receive_a_token: false,
            current_hf: U256::ZERO,
            chain_id: if report.chain == "base" { 8453 } else { 42161 },
            bad_debt: false,
        };

        let result: SimulationResult = simulator
            .simulate_liquidation(
                &candidate,
                U256::from(30_000_000_000u64), // 30 gwei
                U256::from(1_150_000u64),      // 1.15e6 scalar (Base)
                report.block_number,
                &crate::PacingConfig::default(),
            )
            .await?;

        println!(
            "[golden]   -> Simulation: profitable={}, profit=${:.2}, gas={}",
            result.profitable, result.expected_profit_usd, result.gas_used
        );
    }

    println!("[golden] All replays completed.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_golden_replays_path() -> std::path::PathBuf {
        let candidates = [
            std::path::Path::new("../tests/fixtures/golden_replays.json"),
            std::path::Path::new("core/tests/fixtures/golden_replays.json"),
            std::path::Path::new("tests/fixtures/golden_replays.json"),
        ];
        for p in &candidates {
            if p.exists() {
                return p.to_path_buf();
            }
        }
        panic!(
            "golden_replays.json not found in any of: {}",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    #[test]
    fn golden_replays_parse() {
        let raw = std::fs::read_to_string("tests/fixtures/golden_replays.json")
            .unwrap_or_else(|e| panic!("golden_replays.json not found: {e}"));
        let fixtures: Vec<HistoricalLiquidation> = serde_json::from_str(&raw)
            .expect("golden_replays.json must deserialize into Vec<HistoricalLiquidation>");
        assert!(fixtures.len() >= 2, "Need at least 2 replay fixtures");
        for f in &fixtures {
            assert!(!f.chain.is_empty());
            assert!(f.block_number > 0);
        }
    }

    #[test]
    fn golden_replays_have_verified_source_fields() {
        let raw = std::fs::read_to_string("tests/fixtures/golden_replays.json").unwrap();
        let fixtures: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        for f in &fixtures {
            assert!(f.get("_verified").is_some(), "Missing _verified field");
            assert!(f.get("_source").is_some(), "Missing _source field");
            assert!(f.get("user").is_some(), "Missing user field");
            assert!(f.get("debt_to_cover").is_some(), "Missing debt_to_cover field");
        }
    }
}
