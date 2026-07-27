//! Offline falsification model for the Bespoke Tranche Opportunity strategy.
//!
//! Recommended by `docs/decision-2026-07-26-tranche-venue.md` §9.5 as the
//! cheapest form of kill test: no chain access, no REVM, no relay, no capital,
//! no keys.
//!
//! ## Scope
//! This is a **kill test**, not strategy enablement. It answers exactly one
//! question numerically: does a three-leg bundle whose middle leg is the
//! operator's *own* `Executor.execute` liquidation produce positive net
//! revenue?
//!
//! ## The structure under test
//! ```text
//!   leg A (pre-trade)   owner buys collateral from the pool
//!   leg B (liquidation) owner's Executor sells seized collateral to the pool
//!   leg C (post-trade)  owner sells back the collateral bought in leg A
//! ```
//! All three legs share one beneficial owner. The strategy books the resulting
//! price move as revenue.
//!
//! ## The result
//! Under a constant-product invariant the collateral reserve traverses
//! `x0 → x0−dq → x0−dq+liq → x0+liq`. The endpoints are identical to the
//! liquidation alone, and value moved depends only on endpoints, so the round
//! trip contributes **exactly zero** gross. With fees it is strictly negative
//! by the two extra fee crossings — before gas and relay tip.
//!
//! If any assertion in this file ever fails, the strategy's premise has changed
//! and the decision memo needs revisiting. That is the point of keeping it.

use rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// Constant-product pool
// ---------------------------------------------------------------------------

/// A two-asset constant-product pool (`x * y = k`) with a proportional fee.
///
/// `collateral` is the asset the liquidation seizes and sells; `debt` is the
/// asset the liquidation repays. Both are held as [`Decimal`] so the model
/// obeys the repository's no-`f64`-for-money invariant.
#[derive(Debug, Clone, Copy)]
struct Pool {
    collateral: Decimal,
    debt: Decimal,
    fee_bps: u32,
}

impl Pool {
    fn new(collateral: Decimal, debt: Decimal, fee_bps: u32) -> Self {
        Self {
            collateral,
            debt,
            fee_bps,
        }
    }

    /// Fee-adjusted input multiplier, i.e. `1 − fee`.
    fn net_of_fee(&self, amount: Decimal) -> Decimal {
        let fee = Decimal::from(self.fee_bps) / Decimal::from(10_000);
        amount * (Decimal::ONE - fee)
    }

    /// Sell `amount_in` collateral into the pool; returns debt tokens received.
    fn sell_collateral(&mut self, amount_in: Decimal) -> Decimal {
        if amount_in <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let net_in = self.net_of_fee(amount_in);
        let out = (self.debt * net_in) / (self.collateral + net_in);
        self.collateral += amount_in;
        self.debt -= out;
        out
    }

    /// Spend `amount_in` debt tokens; returns collateral received.
    fn buy_collateral(&mut self, amount_in: Decimal) -> Decimal {
        if amount_in <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let net_in = self.net_of_fee(amount_in);
        let out = (self.collateral * net_in) / (self.debt + net_in);
        self.debt += amount_in;
        self.collateral -= out;
        out
    }
}

// ---------------------------------------------------------------------------
// The two scenarios
// ---------------------------------------------------------------------------

/// Baseline: the liquidation alone.
///
/// The Executor sells the seized collateral for the debt asset. Returns the
/// owner's proceeds, denominated in the debt asset.
fn baseline_proceeds(pool: Pool, liquidation_size: Decimal) -> Decimal {
    let mut p = pool;
    p.sell_collateral(liquidation_size)
}

/// Bundle: pre-trade buy → own liquidation → post-trade sell.
///
/// Returns the owner's proceeds in the debt asset. The owner's collateral
/// position is flat in both scenarios: leg C sells exactly what leg A bought,
/// and the seized collateral is sold in leg B either way. So the two scenarios
/// are directly comparable on the debt-asset axis alone.
fn bundle_proceeds(pool: Pool, liquidation_size: Decimal, pre_trade_spend: Decimal) -> Decimal {
    let mut p = pool;

    // Leg A — buy collateral, paying `pre_trade_spend` debt tokens.
    let acquired = p.buy_collateral(pre_trade_spend);

    // Leg B — the Executor sells the seized collateral (direction per
    // contracts/src/Executor.yul: collateral → debt).
    let liquidation_out = p.sell_collateral(liquidation_size);

    // Leg C — sell back exactly what leg A acquired, returning to flat.
    let post_trade_out = p.sell_collateral(acquired);

    liquidation_out + post_trade_out - pre_trade_spend
}

/// Bundle proceeds minus baseline proceeds. Positive would mean the strategy
/// adds value; zero means it is a wash; negative means it destroys value.
fn delta(pool: Pool, liquidation_size: Decimal, pre_trade_spend: Decimal) -> Decimal {
    bundle_proceeds(pool, liquidation_size, pre_trade_spend)
        - baseline_proceeds(pool, liquidation_size)
}

// ---------------------------------------------------------------------------
// Parameter sweep
// ---------------------------------------------------------------------------

fn d(v: i64) -> Decimal {
    Decimal::from(v)
}

/// Three pool shapes: deep-balanced, shallow, and heavily asymmetric.
fn pools(fee_bps: u32) -> Vec<(&'static str, Pool)> {
    vec![
        (
            "deep-balanced",
            Pool::new(d(1_000_000), d(1_000_000), fee_bps),
        ),
        ("shallow", Pool::new(d(5_000), d(5_000), fee_bps)),
        ("asymmetric", Pool::new(d(250_000), d(4_000_000), fee_bps)),
    ]
}

fn liquidation_sizes() -> Vec<Decimal> {
    vec![Decimal::ONE, d(10), d(137)]
}

fn pre_trade_sizes() -> Vec<Decimal> {
    vec![Decimal::new(1, 3), d(3), d(50)] // 0.001, 3, 50
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// With fees switched off, the three-leg bundle is *exactly* a wash.
///
/// This is the load-bearing result. It is an endpoint argument, not an
/// approximation: the pool's collateral reserve ends at `x0 + liq` under both
/// scenarios, so the debt tokens that leave the pool are identical.
#[test]
fn zero_fee_bundle_nets_exactly_zero() {
    // Tolerance covers Decimal's 28-significant-digit division rounding only.
    let epsilon = Decimal::new(1, 18); // 1e-18

    for (name, pool) in pools(0) {
        for liq in liquidation_sizes() {
            for pre in pre_trade_sizes() {
                let delta = delta(pool, liq, pre);
                assert!(
                    delta.abs() < epsilon,
                    "zero-fee delta should be 0 for pool={name} liq={liq} pre={pre}, got {delta}"
                );
            }
        }
    }
}

/// With any real fee tier, the bundle is strictly worse than the plain
/// liquidation — across every pool shape, liquidation size and pre-trade size.
#[test]
fn every_real_fee_tier_makes_the_bundle_strictly_worse() {
    for fee_bps in [1u32, 5, 30, 100] {
        for (name, pool) in pools(fee_bps) {
            for liq in liquidation_sizes() {
                for pre in pre_trade_sizes() {
                    let delta = delta(pool, liq, pre);
                    assert!(
                        delta < Decimal::ZERO,
                        "delta must be negative at {fee_bps}bps for pool={name} \
                         liq={liq} pre={pre}, got {delta}"
                    );
                }
            }
        }
    }
}

/// The loss grows monotonically with the fee tier: it is the two extra fee
/// crossings, nothing else.
#[test]
fn loss_scales_with_fee_tier() {
    let pool_at = |fee| Pool::new(d(1_000_000), d(1_000_000), fee);
    let liq = d(137);
    let pre = d(50);

    let l1 = delta(pool_at(1), liq, pre);
    let l5 = delta(pool_at(5), liq, pre);
    let l30 = delta(pool_at(30), liq, pre);
    let l100 = delta(pool_at(100), liq, pre);

    assert!(l1 > l5, "1bps loss {l1} should be smaller than 5bps {l5}");
    assert!(
        l5 > l30,
        "5bps loss {l5} should be smaller than 30bps {l30}"
    );
    assert!(
        l30 > l100,
        "30bps loss {l30} should be smaller than 100bps {l100}"
    );
}

/// A larger pre-trade does not rescue the strategy — it deepens the loss.
///
/// This closes the "we just need to size it correctly" objection.
#[test]
fn larger_pre_trade_deepens_the_loss() {
    let pool = Pool::new(d(1_000_000), d(1_000_000), 30);
    let liq = d(137);

    let mut previous = Decimal::ZERO;
    for pre in [Decimal::new(1, 3), Decimal::ONE, d(10), d(50), d(500)] {
        let delta = delta(pool, liq, pre);
        assert!(
            delta < Decimal::ZERO,
            "delta must stay negative, got {delta}"
        );
        assert!(
            delta < previous,
            "loss should deepen as pre-trade grows: pre={pre} gave {delta}, \
             previous was {previous}"
        );
        previous = delta;
    }
}

/// The profit gate can never be satisfied.
///
/// `CHIMERA_TRANCHE_MIN_PROFIT_USD` defaults to 1.00 in the operator's
/// environment. Since the gross delta is bounded above by zero and gas plus
/// relay tip are strictly positive, a correctly implemented profit gate rejects
/// 100% of candidates. Building legs to feed such a gate is dead code.
#[test]
fn profit_gate_rejects_every_candidate() {
    let min_profit_usd = Decimal::ONE; // CHIMERA_TRANCHE_MIN_PROFIT_USD=1.00
    let gas_and_tip_usd = Decimal::new(5, 2); // a deliberately tiny $0.05

    let mut checked = 0usize;
    for fee_bps in [1u32, 5, 30, 100] {
        for (_, pool) in pools(fee_bps) {
            for liq in liquidation_sizes() {
                for pre in pre_trade_sizes() {
                    let net = delta(pool, liq, pre) - gas_and_tip_usd;
                    assert!(
                        net < min_profit_usd,
                        "profit gate should reject every candidate, but net={net} \
                         cleared min_profit={min_profit_usd}"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 108, "expected full sweep coverage");
}

/// Sanity check on the model itself: the plain liquidation *is* profitable in
/// the debt asset, so a negative delta reflects the bundle destroying value —
/// not a broken pool model.
#[test]
fn baseline_liquidation_is_itself_productive() {
    let pool = Pool::new(d(1_000_000), d(1_000_000), 30);
    let proceeds = baseline_proceeds(pool, d(137));
    assert!(
        proceeds > Decimal::ZERO,
        "selling seized collateral should yield debt tokens, got {proceeds}"
    );
    // Slippage plus fee means proceeds land just under the 1:1 spot rate.
    assert!(
        proceeds < d(137),
        "proceeds {proceeds} should reflect slippage"
    );
}

/// Reversing the legs into the genuinely profitable orientation moves value
/// between the operator's own accounts and nets to zero minus fees.
///
/// Guards against "just flip the direction" as a proposed fix.
#[test]
fn reversing_leg_direction_still_nets_negative() {
    let pool = Pool::new(d(1_000_000), d(1_000_000), 30);
    let liq = d(137);
    let pre = d(50);

    // Reversed: sell first, buy back after the liquidation.
    let reversed = {
        let mut p = pool;
        let sold_for = p.sell_collateral(pre);
        let liquidation_out = p.sell_collateral(liq);
        let bought_back = p.buy_collateral(sold_for);
        // Value the returned collateral at the pool's post-trade spot rate.
        let spot = p.debt / p.collateral;
        liquidation_out + (bought_back * spot) - (pre * (p.debt / p.collateral))
    };

    let base = baseline_proceeds(pool, liq);
    assert!(
        reversed < base,
        "reversed orientation {reversed} should not beat baseline {base}"
    );
}

/// Prints the sweep so the numbers are inspectable rather than only asserted.
/// Run with: `cargo test -p chimera-core --test tranche_falsification_test -- --nocapture report`
#[test]
fn report() {
    let pool_at = |fee| Pool::new(d(1_000_000), d(1_000_000), fee);
    let liq = d(137);
    let pre = d(50);
    println!("\n  deep-balanced pool 1e6/1e6, liquidation=137, pre-trade=50");
    println!("  {:<10} {:>22}", "fee", "bundle - baseline");
    for fee in [0u32, 1, 5, 30, 100] {
        println!(
            "  {:<10} {:>22}",
            format!("{fee}bps"),
            delta(pool_at(fee), liq, pre)
        );
    }
    println!();
}

// ---------------------------------------------------------------------------
// The trap
// ---------------------------------------------------------------------------

/// Leg B's swap output and the owner's actual P&L move in **opposite
/// directions** as the pre-trade grows.
///
/// This is the most dangerous property of the structure, and the reason the
/// numbers are seductive. The pre-buy genuinely does raise the price the
/// Executor sells into, so `Executor.yul`'s own profit event
/// (`Executor.yul:204-206`) reports a *larger* number as the pre-trade grows.
/// The loss lives entirely in legs A and C — outside the contract, and outside
/// its telemetry.
///
/// An operator instrumenting only the Executor would watch the metric climb
/// while the account drains. Anything that reports "tranche profit" must be
/// measured on the owner's net across all three legs, never on the swap return.
#[test]
fn executor_telemetry_rises_while_owner_loses() {
    // Base-realistic: 1200 WETH / 3.6M USDC, 30bps.
    let pool = Pool::new(d(1_200), d(3_600_000), 30);
    let liq = d(20); // seized collateral

    let leg_b_output = |pre: Decimal| -> Decimal {
        let mut p = pool;
        p.buy_collateral(pre);
        p.sell_collateral(liq)
    };

    let mut prev_leg_b = Decimal::ZERO;
    let mut prev_owner = Decimal::MAX;

    for pre in [Decimal::ZERO, Decimal::ONE, d(20), d(100), d(300)] {
        let leg_b = leg_b_output(pre);
        let owner = delta(pool, liq, pre);

        assert!(
            leg_b > prev_leg_b,
            "leg B output should RISE with pre-trade (the trap): pre={pre} \
             gave {leg_b}, previous {prev_leg_b}"
        );
        assert!(
            owner < prev_owner,
            "owner P&L should FALL as leg B output rises: pre={pre} gave \
             {owner}, previous {prev_owner}"
        );

        prev_leg_b = leg_b;
        prev_owner = owner;
    }
}

/// The Executor's `minProfit` gate is not a defence against this structure.
///
/// `Executor.yul:193-202` reverts unless
/// `balanceAfter > balanceBefore + premium + minProfit + tip`. Because the
/// pre-buy inflates leg B's output, it inflates `balanceAfter` — so a
/// liquidation that would have safely reverted on thin margin can be pushed
/// *through* the gate by wrapping it. The gate sees a healthier trade; the
/// owner still loses on the round trip.
#[test]
fn profit_gate_can_be_pushed_through_by_the_wrapper() {
    let pool = Pool::new(d(1_200), d(3_600_000), 30);
    let liq = d(20);

    let bare = {
        let mut p = pool;
        p.sell_collateral(liq)
    };

    let wrapped = {
        let mut p = pool;
        p.buy_collateral(d(300));
        p.sell_collateral(liq)
    };

    // The gate's input is strictly larger when wrapped ...
    assert!(
        wrapped > bare,
        "wrapper should inflate the gate's balanceAfter: {wrapped} vs {bare}"
    );
    // ... while the owner is strictly worse off.
    assert!(
        delta(pool, liq, d(300)) < Decimal::ZERO,
        "owner must still be worse off despite the healthier-looking gate"
    );
}
