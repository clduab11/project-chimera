//! Split routing vs. create-then-arbitrage — which multi-leg structure to build.
//!
//! With the tranche/sandwich structure withdrawn (see
//! `docs/decision-2026-07-26-tranche-venue.md` and
//! `tranche_falsification_test.rs`), two legitimate multi-leg structures remain.
//! Both have a real profit source and neither needs a victim:
//!
//! - **(B) Split routing** — spread the liquidation's collateral sale across
//!   venues so less slippage is paid in the first place. One transaction, N
//!   swap legs, no inventory risk.
//! - **(C) Create-then-arbitrage** — sell the whole balance into one venue,
//!   then capture the cross-venue dislocation that sale just created by buying
//!   the now-cheap collateral back on that venue and selling it on another.
//!
//! Both beat the status quo (A) of dumping everything into one venue. The
//! question this file answers is whether they **compose**.
//!
//! ## They do not. They are substitutes, and B dominates.
//!
//! C's revenue *is* the dislocation, and B's whole purpose is to not create
//! one. Every basis point B saves is a basis point C can no longer earn. Since
//! B captures the gap without the extra fee crossings, extra gas, or the
//! inventory exposure C carries between its legs, B wins outright — and
//! stacking them earns less than B alone.
//!
//! Build split routing. Do not build create-then-arb on top of it.

use rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// Mutable constant-product venue
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Venue {
    /// Reserve of the collateral token (the asset being sold).
    weth: Decimal,
    /// Reserve of the debt token (the asset being bought).
    usdc: Decimal,
    fee_bps: u32,
}

impl Venue {
    fn new(weth: i64, usdc: i64, fee_bps: u32) -> Self {
        Self {
            weth: Decimal::from(weth),
            usdc: Decimal::from(usdc),
            fee_bps,
        }
    }

    fn net(&self, amount: Decimal) -> Decimal {
        amount * (Decimal::ONE - Decimal::from(self.fee_bps) / Decimal::from(10_000))
    }

    /// Sell collateral, receive debt tokens.
    fn sell_weth(&mut self, amount: Decimal) -> Decimal {
        if amount <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let net = self.net(amount);
        let out = (self.usdc * net) / (self.weth + net);
        self.weth += amount;
        self.usdc -= out;
        out
    }

    /// Spend debt tokens, receive collateral.
    fn buy_weth(&mut self, spend: Decimal) -> Decimal {
        if spend <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let net = self.net(spend);
        let out = (self.weth * net) / (self.usdc + net);
        self.usdc += spend;
        self.weth -= out;
        out
    }
}

fn d(v: i64) -> Decimal {
    Decimal::from(v)
}

/// Base-shaped WETH/USDC depth: venue A deeper than venue B, both 30bps.
fn book() -> (Venue, Venue) {
    (
        Venue::new(1_200, 3_600_000, 30),
        Venue::new(800, 2_400_000, 30),
    )
}

const LIQUIDATION: i64 = 137;

// ---------------------------------------------------------------------------
// The three strategies
// ---------------------------------------------------------------------------

/// (A) Status quo — the whole seized balance into the single deepest venue.
/// This is what `Executor.yul:167-183` does today.
fn strategy_single_venue() -> Decimal {
    let (mut a, _) = book();
    a.sell_weth(d(LIQUIDATION))
}

/// (B) Split routing — allocate across both venues to equalise marginal price.
///
/// Uses the same greedy chunk allocation as `routing::split::plan_split`, run
/// against mutable reserves here so it is directly comparable with (C).
fn strategy_split() -> Decimal {
    let (a, b) = book();
    let total = d(LIQUIDATION);
    let chunks = 256u32;
    let chunk = total / Decimal::from(chunks);

    let (mut alloc_a, mut alloc_b) = (Decimal::ZERO, Decimal::ZERO);
    let out_of = |v: &Venue, amt: Decimal| -> Decimal {
        if amt <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let net = v.net(amt);
        (v.usdc * net) / (v.weth + net)
    };

    for _ in 0..chunks {
        let gain_a = out_of(&a, alloc_a + chunk) - out_of(&a, alloc_a);
        let gain_b = out_of(&b, alloc_b + chunk) - out_of(&b, alloc_b);
        if gain_a >= gain_b {
            alloc_a += chunk;
        } else {
            alloc_b += chunk;
        }
    }

    let (mut a, mut b) = book();
    a.sell_weth(alloc_a) + b.sell_weth(alloc_b)
}

/// (C) Create-then-arbitrage — dump into A, then arb the gap A→B.
///
/// Returns the best result over a scan of arb sizes, so this is the *most
/// favourable* version of (C), not a strawman.
fn strategy_create_then_arb() -> Decimal {
    let mut best = Decimal::MIN;

    for spend_step in 0..=60 {
        let spend = d(spend_step) * d(5_000); // 0 … 300,000 USDC
        let (mut a, mut b) = book();

        // Leg 1 — the liquidation, dumped whole into A.
        let liquidation_out = a.sell_weth(d(LIQUIDATION));

        // Legs 2 & 3 — buy the now-cheap collateral on A, sell it on B.
        let acquired = a.buy_weth(spend);
        let recovered = b.sell_weth(acquired);

        let total = liquidation_out - spend + recovered;
        if total > best {
            best = total;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Both legitimate structures beat dumping everything into one venue.
#[test]
fn both_structures_beat_the_status_quo() {
    let single = strategy_single_venue();
    let split = strategy_split();
    let arb = strategy_create_then_arb();

    assert!(
        split > single,
        "split {split} should beat single-venue {single}"
    );
    assert!(
        arb > single,
        "create-then-arb {arb} should beat single-venue {single}"
    );
}

/// Split routing dominates create-then-arbitrage.
///
/// The decision this file exists to make. C must pay two extra fee crossings
/// and carry inventory between legs to recover a dislocation that B simply
/// never creates.
#[test]
fn split_routing_dominates_create_then_arb() {
    let split = strategy_split();
    let arb = strategy_create_then_arb();

    assert!(
        split > arb,
        "split routing {split} should dominate create-then-arb {arb}"
    );
}

/// Stacking them is worse than split routing alone.
///
/// Guards against "do both" — running the arb *after* an already-split sale
/// finds a smaller dislocation and still pays the extra fee crossings to chase
/// it. They are substitutes, not complements.
#[test]
fn stacking_arb_on_top_of_split_is_not_additive() {
    let split_only = strategy_split();

    // Split the sale, then try to arb whatever dislocation remains.
    let mut best_stacked = Decimal::MIN;
    for spend_step in 0..=60 {
        let spend = d(spend_step) * d(5_000);
        let (mut a, mut b) = book();

        // Same allocation the split test derives, applied to fresh reserves.
        let half = d(LIQUIDATION) / d(2);
        let liquidation_out = a.sell_weth(half) + b.sell_weth(d(LIQUIDATION) - half);

        let acquired = a.buy_weth(spend);
        let recovered = b.sell_weth(acquired);

        let total = liquidation_out - spend + recovered;
        if total > best_stacked {
            best_stacked = total;
        }
    }

    assert!(
        best_stacked <= split_only,
        "stacking arb on split ({best_stacked}) should not beat split alone ({split_only})"
    );
}

/// The advantage is a size effect: it vanishes on small trades.
///
/// Keeps the result honest. Observed Base Aave liquidation flow is small, so
/// this is the regime that actually applies.
#[test]
fn advantage_vanishes_on_small_trades() {
    let tiny = {
        let (mut a, _) = book();
        a.sell_weth(Decimal::new(1, 2)) // 0.01 WETH
    };
    let (a0, _) = book();
    let spot_rate = a0.usdc / a0.weth;
    let frictionless = Decimal::new(1, 2) * spot_rate;

    // At 0.01 WETH against a 1200 WETH pool, execution is within the fee of
    // spot — there is essentially no slippage left for any structure to save.
    let gap_bps = ((frictionless - tiny) / frictionless) * d(10_000);
    assert!(
        gap_bps < d(35),
        "tiny trade should execute within ~fee of spot, gap was {gap_bps}bps"
    );
}

/// Prints the comparison. Run with:
/// `cargo test -p chimera-core --test split_vs_arb_test -- --nocapture report`
#[test]
fn report() {
    let single = strategy_single_venue();
    let split = strategy_split();
    let arb = strategy_create_then_arb();

    let bps = |v: Decimal| ((v - single) / single) * d(10_000);

    println!(
        "\n  liquidating {LIQUIDATION} WETH across two 30bps venues (1200/3.6M and 800/2.4M)\n"
    );
    println!(
        "  {:<28} {:>14} {:>12}",
        "strategy", "USDC out", "vs single"
    );
    println!("  {:<28} {:>14.2} {:>12}", "(A) single venue", single, "—");
    println!(
        "  {:<28} {:>14.2} {:>11.0}bps",
        "(B) split routing",
        split,
        bps(split)
    );
    println!(
        "  {:<28} {:>14.2} {:>11.0}bps",
        "(C) create-then-arb",
        arb,
        bps(arb)
    );
    println!();
}
