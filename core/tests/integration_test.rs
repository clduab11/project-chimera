//! Integration test: orchestrator pacing + detector + EOA validation
//! Run: cargo test --test integration_test

use chimera_core::{Opportunity, PacingConfig, PacingEngine};
use rust_decimal::Decimal;

#[test]
fn orchestrator_pacing_allows_valid_liquidation() {
    let config = PacingConfig::default(); // 2000 daily, 7500 weekly, 1000 single
    let engine = PacingEngine::new(config);

    let opp = Opportunity {
        id: "integration-001".into(),
        expected_net_usd: Decimal::new(450, 0), // $450 profit
        gas_estimate_gwei: 30,
        venue: "aerodrome".into(),
        eoa: "0x1111111111111111111111111111111111111111".into(),
        timestamp: chrono::Utc::now(),
    };

    let decision = engine.check(&opp).unwrap();
    assert!(
        matches!(decision, chimera_core::PacingDecision::Allow { .. }),
        "Valid liquidation should pass pacing gate"
    );
}

#[test]
fn orchestrator_pacing_denies_over_single_cap() {
    let config = PacingConfig::default();
    let engine = PacingEngine::new(config);

    let opp = Opportunity {
        id: "integration-002".into(),
        expected_net_usd: Decimal::new(2000, 0), // $2000 > $1000 single cap
        gas_estimate_gwei: 30,
        venue: "aerodrome".into(),
        eoa: "0x1111111111111111111111111111111111111111".into(),
        timestamp: chrono::Utc::now(),
    };

    let decision = engine.check(&opp).unwrap();
    assert!(
        matches!(decision, chimera_core::PacingDecision::Deny { .. }),
        "Over-cap liquidation should be denied"
    );
}
