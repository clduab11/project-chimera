//! Multi-venue split routing for the liquidation swap leg.
//!
//! The Executor sells 100% of the seized collateral for the debt asset in a
//! single swap against a single router (`contracts/src/Executor.yul:167-183`).
//! On a constant-product venue the marginal execution price decays with size,
//! so dumping the whole balance into one pool pays avoidable slippage.
//!
//! Splitting the same sale across several venues strictly improves proceeds
//! whenever more than one venue has depth in the pair. Unlike the withdrawn
//! tranche structure, the profit source here is real and requires no
//! counterparty: it is slippage **not paid**, on a trade that happens anyway.
//! There are no extra fee crossings — the same notional is sold either way.
//!
//! ## Status
//! This module plans the split. It does **not** execute it: `Executor.yul` has
//! a fixed dispatch table with one `dexRouter` argument and no loop, so
//! executing a multi-venue split needs a contract change and a fresh audit. The
//! planner is useful before that lands — [`SplitPlan::amount_out_min`] tightens
//! the bound on the existing single-venue path, and the plan is the input the
//! contract work will need regardless.
//!
//! ## Method
//! Output is concave in input for every constant-product venue, so the total is
//! maximised by equalising marginal execution price across venues. This is
//! solved by greedy allocation over `granularity` equal chunks rather than
//! closed-form Lagrange multipliers, which would need a square root that
//! [`Decimal`] does not provide. Error is `O(1/granularity)` and the objective
//! is concave, so the greedy result converges from below.
//!
//! ## Sizing expectations — read before quoting a number
//! The gain is a function of **trade size relative to pool depth**, and nothing
//! else. It goes to zero as the trade shrinks: the `report` test shows 0 bps at
//! 1 WETH against those pools, 28 bps at 10, and 1345 bps at 400. Those pools
//! are deliberately shallow to make the mechanism visible — real Base WETH/USDC
//! depth is far deeper, and observed Base Aave liquidation flow is small, so
//! realistic gains sit at the **low end of that range**, not the high end.
//!
//! This improves the take on existing flow. It does not create flow, and it is
//! not a revenue forecast. Quote `improvement_bps` from live reserves for the
//! actual trade, never a figure from the report test.

use alloy::primitives::Address;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Default number of chunks used to discretise the allocation search.
///
/// 256 puts the gap to the continuous optimum well inside a basis point for
/// realistic reserve ratios, at 256 × `venues` marginal evaluations.
pub const DEFAULT_GRANULARITY: u32 = 256;

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// Observed depth for one venue on the collateral → debt pair.
///
/// Reserves are in the pair's native units and must come from a live source
/// (`SnapshotRefresher`), not from config. A stale reserve produces a
/// confidently wrong split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueLiquidity {
    /// Venue name, matching `config/routing.yaml`.
    pub venue_name: String,
    /// V2 router address for this venue.
    pub router: Address,
    /// Reserve of the token being sold (collateral).
    pub reserve_in: Decimal,
    /// Reserve of the token being bought (debt asset).
    pub reserve_out: Decimal,
    /// Venue fee in basis points (30 = 0.30%).
    pub fee_bps: u32,
}

impl VenueLiquidity {
    /// Output for selling `amount_in`, per the constant-product formula with
    /// this venue's fee. Returns zero for non-positive or degenerate input.
    pub fn amount_out(&self, amount_in: Decimal) -> Decimal {
        if amount_in <= Decimal::ZERO
            || self.reserve_in <= Decimal::ZERO
            || self.reserve_out <= Decimal::ZERO
        {
            return Decimal::ZERO;
        }
        let fee = Decimal::from(self.fee_bps) / Decimal::from(10_000);
        let net_in = amount_in * (Decimal::ONE - fee);
        let denominator = self.reserve_in + net_in;
        if denominator <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.reserve_out * net_in) / denominator
    }

    /// Whether this venue is usable in a split.
    fn is_viable(&self) -> bool {
        self.reserve_in > Decimal::ZERO && self.reserve_out > Decimal::ZERO && self.fee_bps < 10_000
    }
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// One venue's share of a split sale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitLeg {
    /// Venue name, matching `config/routing.yaml`.
    pub venue_name: String,
    /// V2 router address to call for this leg.
    pub router: Address,
    /// Amount of collateral to sell on this venue.
    pub amount_in: Decimal,
    /// Expected debt-asset output from this leg.
    pub expected_out: Decimal,
}

/// A planned split of one collateral sale across venues.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitPlan {
    /// Legs with a non-zero allocation, ordered by descending size.
    pub legs: Vec<SplitLeg>,
    /// Total expected output across all legs.
    pub total_out: Decimal,
    /// Output the best single venue would have produced for the whole amount.
    pub best_single_out: Decimal,
    /// Name of that best single venue — the status-quo choice.
    pub best_single_venue: String,
    /// Improvement of the split over the best single venue, in basis points.
    pub improvement_bps: u32,
}

impl SplitPlan {
    /// Slippage-adjusted floor for the whole sale, in the debt asset's units.
    ///
    /// Mirrors the tolerance the resolver applies to a single-venue route, so
    /// this can tighten `amountOutMin` on the existing path before multi-venue
    /// execution exists.
    pub fn amount_out_min(&self, slippage_bps: u32) -> Decimal {
        let tolerance = Decimal::from(slippage_bps.min(10_000)) / Decimal::from(10_000);
        self.total_out * (Decimal::ONE - tolerance)
    }

    /// Whether the split beats the best single venue by at least `min_bps`.
    ///
    /// Executing a split costs extra gas per leg, so a plan that improves by
    /// less than the gas differential is not worth executing.
    pub fn is_worth_executing(&self, min_bps: u32) -> bool {
        self.legs.len() > 1 && self.improvement_bps >= min_bps
    }
}

// ---------------------------------------------------------------------------
// Planner
// ---------------------------------------------------------------------------

/// Plan the allocation of `total_in` collateral across `venues`.
///
/// Returns `None` if `total_in` is non-positive or no venue is viable.
/// A single viable venue yields a one-leg plan with `improvement_bps == 0`,
/// which is the correct answer rather than an error.
pub fn plan_split(
    venues: &[VenueLiquidity],
    total_in: Decimal,
    granularity: u32,
) -> Option<SplitPlan> {
    if total_in <= Decimal::ZERO {
        return None;
    }
    let viable: Vec<&VenueLiquidity> = venues.iter().filter(|v| v.is_viable()).collect();
    if viable.is_empty() {
        return None;
    }

    // Status quo: the whole sale into whichever single venue is deepest.
    let (best_single_idx, best_single_out) = viable
        .iter()
        .enumerate()
        .map(|(i, v)| (i, v.amount_out(total_in)))
        .fold((0usize, Decimal::ZERO), |acc, (i, out)| {
            if out > acc.1 {
                (i, out)
            } else {
                acc
            }
        });
    let best_single_venue = viable[best_single_idx].venue_name.clone();

    // Greedy allocation: hand each chunk to whichever venue pays the most for
    // it *given what that venue has already absorbed*. Concavity makes this
    // converge to the marginal-price-equalising optimum.
    let chunks = granularity.max(1);
    let chunk = total_in / Decimal::from(chunks);
    let mut allocated = vec![Decimal::ZERO; viable.len()];

    for _ in 0..chunks {
        let mut best_idx = 0usize;
        let mut best_gain = Decimal::MIN;
        for (i, venue) in viable.iter().enumerate() {
            let gain = venue.amount_out(allocated[i] + chunk) - venue.amount_out(allocated[i]);
            if gain > best_gain {
                best_gain = gain;
                best_idx = i;
            }
        }
        allocated[best_idx] += chunk;
    }

    let mut legs: Vec<SplitLeg> = viable
        .iter()
        .enumerate()
        .filter(|(i, _)| allocated[*i] > Decimal::ZERO)
        .map(|(i, venue)| SplitLeg {
            venue_name: venue.venue_name.clone(),
            router: venue.router,
            amount_in: allocated[i],
            expected_out: venue.amount_out(allocated[i]),
        })
        .collect();
    legs.sort_by_key(|l| std::cmp::Reverse(l.amount_in));

    let total_out: Decimal = legs.iter().map(|l| l.expected_out).sum();

    // Greedy converges from below, so clamp rather than report a negative
    // improvement from discretisation noise.
    let improvement_bps = if best_single_out > Decimal::ZERO && total_out > best_single_out {
        (((total_out - best_single_out) / best_single_out) * Decimal::from(10_000))
            .try_into()
            .unwrap_or(0u32)
    } else {
        0
    };

    Some(SplitPlan {
        legs,
        total_out,
        best_single_out,
        best_single_venue,
        improvement_bps,
    })
}

/// [`plan_split`] with [`DEFAULT_GRANULARITY`].
pub fn plan_split_default(venues: &[VenueLiquidity], total_in: Decimal) -> Option<SplitPlan> {
    plan_split(venues, total_in, DEFAULT_GRANULARITY)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn d(v: i64) -> Decimal {
        Decimal::from(v)
    }

    fn venue(name: &str, reserve_in: i64, reserve_out: i64, fee_bps: u32) -> VenueLiquidity {
        VenueLiquidity {
            venue_name: name.into(),
            router: address!("0x1111111111111111111111111111111111111111"),
            reserve_in: d(reserve_in),
            reserve_out: d(reserve_out),
            fee_bps,
        }
    }

    #[test]
    fn split_beats_best_single_venue_on_comparable_pools() {
        let venues = vec![
            venue("a", 1_000, 3_000_000, 30),
            venue("b", 900, 2_700_000, 30),
        ];
        let plan = plan_split_default(&venues, d(100)).expect("plan");

        assert!(plan.legs.len() > 1, "should use both venues");
        assert!(
            plan.total_out > plan.best_single_out,
            "split {} should beat single {}",
            plan.total_out,
            plan.best_single_out
        );
        assert!(plan.improvement_bps > 0);
    }

    #[test]
    fn allocation_conserves_the_input_amount() {
        let venues = vec![
            venue("a", 1_000, 3_000_000, 30),
            venue("b", 500, 1_500_000, 5),
            venue("c", 2_000, 6_000_000, 100),
        ];
        let total = d(250);
        let plan = plan_split(&venues, total, 512).expect("plan");

        let allocated: Decimal = plan.legs.iter().map(|l| l.amount_in).sum();
        assert_eq!(
            allocated, total,
            "every unit must be allocated exactly once"
        );
    }

    #[test]
    fn deeper_venue_receives_the_larger_share() {
        let venues = vec![
            venue("shallow", 100, 300_000, 30),
            venue("deep", 10_000, 30_000_000, 30),
        ];
        let plan = plan_split_default(&venues, d(500)).expect("plan");

        let deep = plan.legs.iter().find(|l| l.venue_name == "deep").unwrap();
        let shallow = plan
            .legs
            .iter()
            .find(|l| l.venue_name == "shallow")
            .unwrap();
        assert!(
            deep.amount_in > shallow.amount_in,
            "deep {} should absorb more than shallow {}",
            deep.amount_in,
            shallow.amount_in
        );
    }

    #[test]
    fn cheaper_fee_tier_is_preferred_at_equal_depth() {
        let venues = vec![
            venue("expensive", 1_000, 3_000_000, 100),
            venue("cheap", 1_000, 3_000_000, 5),
        ];
        let plan = plan_split_default(&venues, d(50)).expect("plan");

        let cheap = plan.legs.iter().find(|l| l.venue_name == "cheap").unwrap();
        assert!(
            cheap.amount_in > d(25),
            "cheaper venue should take more than half, got {}",
            cheap.amount_in
        );
    }

    #[test]
    fn single_venue_yields_a_one_leg_plan_with_no_improvement() {
        let venues = vec![venue("only", 1_000, 3_000_000, 30)];
        let plan = plan_split_default(&venues, d(100)).expect("plan");

        assert_eq!(plan.legs.len(), 1);
        assert_eq!(plan.improvement_bps, 0);
        assert_eq!(plan.total_out, plan.best_single_out);
        assert!(!plan.is_worth_executing(1), "one leg is never a split");
    }

    #[test]
    fn improvement_grows_with_trade_size() {
        let venues = vec![
            venue("a", 1_000, 3_000_000, 30),
            venue("b", 1_000, 3_000_000, 30),
        ];
        let small = plan_split_default(&venues, d(1)).expect("plan");
        let large = plan_split_default(&venues, d(400)).expect("plan");

        assert!(
            large.improvement_bps > small.improvement_bps,
            "splitting should matter more at size: {}bps at 400 vs {}bps at 1",
            large.improvement_bps,
            small.improvement_bps
        );
    }

    #[test]
    fn rejects_non_positive_and_degenerate_inputs() {
        let venues = vec![venue("a", 1_000, 3_000_000, 30)];
        assert!(plan_split_default(&venues, Decimal::ZERO).is_none());
        assert!(plan_split_default(&venues, d(-5)).is_none());
        assert!(plan_split_default(&[], d(100)).is_none());

        let empty_pool = vec![venue("drained", 0, 0, 30)];
        assert!(plan_split_default(&empty_pool, d(100)).is_none());
    }

    #[test]
    fn skips_venues_with_no_depth_without_failing() {
        let venues = vec![
            venue("live", 1_000, 3_000_000, 30),
            venue("drained", 0, 0, 30),
        ];
        let plan = plan_split_default(&venues, d(100)).expect("plan");
        assert_eq!(plan.legs.len(), 1);
        assert_eq!(plan.legs[0].venue_name, "live");
    }

    #[test]
    fn amount_out_min_applies_slippage_tolerance() {
        let venues = vec![venue("a", 1_000, 3_000_000, 30)];
        let plan = plan_split_default(&venues, d(100)).expect("plan");

        let floor = plan.amount_out_min(100); // 1%
        assert!(floor < plan.total_out);
        assert_eq!(floor, plan.total_out * Decimal::new(99, 2));
        // A 100% tolerance floors at zero rather than going negative.
        assert_eq!(plan.amount_out_min(10_000), Decimal::ZERO);
    }

    #[test]
    fn is_worth_executing_respects_the_gas_threshold() {
        let venues = vec![
            venue("a", 1_000, 3_000_000, 30),
            venue("b", 1_000, 3_000_000, 30),
        ];
        let plan = plan_split_default(&venues, d(400)).expect("plan");

        assert!(plan.is_worth_executing(1));
        assert!(
            !plan.is_worth_executing(plan.improvement_bps + 1),
            "a threshold above the improvement must reject"
        );
    }

    #[test]
    fn higher_granularity_does_not_reduce_output() {
        let venues = vec![
            venue("a", 1_000, 3_000_000, 30),
            venue("b", 1_400, 4_000_000, 5),
        ];
        let coarse = plan_split(&venues, d(300), 16).expect("plan");
        let fine = plan_split(&venues, d(300), 1024).expect("plan");

        assert!(
            fine.total_out >= coarse.total_out,
            "finer granularity {} should not be worse than coarse {}",
            fine.total_out,
            coarse.total_out
        );
    }

    /// Prints realistic split gains. Run with:
    /// `cargo test -p chimera-core --lib routing::split::tests::report -- --nocapture`
    #[test]
    fn report() {
        // Base-shaped WETH/USDC depth across three venues at their real fee tiers.
        let venues = vec![
            venue("uniswap-v3-base", 1_200, 3_600_000, 5),
            venue("aerodrome-base", 800, 2_400_000, 30),
            venue("sushi-base", 300, 900_000, 30),
        ];
        println!(
            "
  WETH sold   single-venue out       split out    gain(bps)   legs"
        );
        for size in [d(1), d(10), d(50), d(137), d(400)] {
            let p = plan_split_default(&venues, size).expect("plan");
            println!(
                "  {:>9}   {:>16.2}   {:>13.2}   {:>9}   {:>4}",
                size,
                p.best_single_out,
                p.total_out,
                p.improvement_bps,
                p.legs.len()
            );
        }
        println!();
    }
}
