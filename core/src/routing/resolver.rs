//! RoutingResolver — resolves collateral/debt pairs to V2 DEX routes.
//!
//! Consumes the [`RoutingConfig`] (loaded from `config/routing.yaml`) to find a
//! V2-compatible DEX venue with a matching trading pair. Computes `amountOutMin`
//! from `slippage_max_bps` in the [`RiskConfig`].
//!
//! Scope: V2-only routing. V3/Aerodrome routing is out of scope for this Epic.

use crate::config::{RiskConfig, RoutingConfig, TradingPair};
use alloy::primitives::{Address, U256};
use std::str::FromStr;
use tracing::{info, warn};

/// A resolved V2 DEX route for a collateral/debt pair.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedV2Route {
    /// V2 DEX router address (target of the swap call).
    pub router: Address,
    /// Token path for the swap (e.g. `[debt_asset, collateral_asset]`).
    /// For a single-hop V2 route, this is 2 addresses: input and output.
    pub path: Vec<Address>,
    /// Minimum output amount for the swap leg, computed from
    /// `debt_to_cover` adjusted by flash-loan premium and `slippage_max_bps`.
    ///
    /// The Executor swaps collateral → debt, so `amountOutMin` is
    /// denominated in the debt-asset's units.
    pub amount_out_min: U256,
    /// Name of the DEX venue that resolved the route.
    pub venue_name: String,
}

/// Resolves collateral/debt pairs to V2 DEX routes from the routing configuration.
///
/// # V2-only scope
/// This resolver only matches venues with `type: "dex"` and a populated
/// `router_address` + `pairs`. V3/Aerodrome-style routing is deferred.
pub struct RoutingResolver<'a> {
    routing: &'a RoutingConfig,
    risk: &'a RiskConfig,
}

impl<'a> RoutingResolver<'a> {
    /// Create a new resolver backed by the given routing and risk configs.
    pub fn new(routing: &'a RoutingConfig, risk: &'a RiskConfig) -> Self {
        Self { routing, risk }
    }

    /// Resolve a V2 route for the given collateral and debt assets on a specific chain.
    ///
    /// Searches all configured V2 venues for the chain, finds the first venue with
    /// a matching trading pair, and returns a [`ResolvedV2Route`] with the router
    /// address, token path, and slippage-adjusted minimum output.
    ///
    /// Equivalent to [`Self::resolve_v2_eligible`] with every venue eligible.
    ///
    /// # Returns
    /// - `Some(ResolvedV2Route)` if a matching V2 route is found.
    /// - `None` if no matching route exists (candidate will be skipped).
    pub fn resolve_v2(
        &self,
        collateral: Address,
        debt: Address,
        chain_label: &str,
        debt_to_cover: U256,
    ) -> Option<ResolvedV2Route> {
        self.resolve_v2_eligible(collateral, debt, chain_label, debt_to_cover, |_| true)
    }

    /// Rotation-aware resolution: like [`Self::resolve_v2`], but a venue whose
    /// name fails the `is_eligible` predicate is skipped and resolution falls
    /// through to the next matching venue.
    ///
    /// The caller passes a predicate over venue names — typically
    /// `|venue| !recent_venues.contains(venue)` built from
    /// `PacingEngine::recent_venues()` — so the resolver never returns a venue
    /// the pacing rotation gate would deny post-hoc (which previously dropped
    /// the candidate instead of re-routing it).
    ///
    /// # Returns
    /// - `Some(ResolvedV2Route)` for the first eligible venue with a matching pair.
    /// - `None` if no eligible matching route exists (candidate will be skipped).
    pub fn resolve_v2_eligible(
        &self,
        collateral: Address,
        debt: Address,
        chain_label: &str,
        debt_to_cover: U256,
        is_eligible: impl Fn(&str) -> bool,
    ) -> Option<ResolvedV2Route> {
        let venues = self.routing.venues_for_chain(chain_label);
        let mut ineligible_matches: u32 = 0;

        for venue in venues {
            // Only V2-compatible DEX venues with a router address are in scope.
            if venue.venue_type != "dex"
                || venue.router_compatibility != "v2"
                || venue.router_address.is_empty()
            {
                continue;
            }

            let router = match Address::from_str(&venue.router_address) {
                Ok(a) => a,
                Err(_) => {
                    warn!(
                        target = "chimera::routing",
                        venue = %venue.name,
                        router = %venue.router_address,
                        "Invalid router address in routing config; skipping venue"
                    );
                    continue;
                }
            };

            // Check if any trading pair matches our collateral→debt swap direction.
            // The Executor swaps seized collateral → debt;
            // pairs in routing.yaml are declared as collateral→debt.
            if let Some(pair) = Self::find_matching_pair(&venue.pairs, collateral, debt) {
                if !is_eligible(&venue.name) {
                    ineligible_matches += 1;
                    info!(
                        target = "chimera::routing",
                        venue = %venue.name,
                        "Venue has a matching route but is rotation-blocked; trying next venue"
                    );
                    continue;
                }

                let amount_out_min =
                    Self::compute_amount_out_min(debt_to_cover, self.risk.slippage_max_bps);

                return Some(ResolvedV2Route {
                    router,
                    path: vec![pair.0, pair.1],
                    amount_out_min,
                    venue_name: venue.name.clone(),
                });
            }
        }

        warn!(
            target = "chimera::routing",
            %collateral,
            %debt,
            chain = %chain_label,
            rotation_blocked = ineligible_matches,
            "No eligible V2 route found for collateral/debt pair"
        );
        None
    }

    /// Find a matching trading pair. Returns `Some((collateral, debt))` if:
    /// - `token_in` == collateral AND `token_out` == debt.
    ///
    /// This matches the routing.yaml config where pairs are declared as collateral→debt,
    /// consistent with the Executor swap from seized collateral to debt.
    fn find_matching_pair(
        pairs: &[TradingPair],
        collateral: Address,
        debt: Address,
    ) -> Option<(Address, Address)> {
        for pair in pairs {
            let token_in = match Address::from_str(&pair.token_in) {
                Ok(a) => a,
                Err(_) => continue,
            };
            let token_out = match Address::from_str(&pair.token_out) {
                Ok(a) => a,
                Err(_) => continue,
            };

            // Match: collateral → debt (the Executor swaps seized collateral to debt)
            if token_in == collateral && token_out == debt {
                return Some((token_in, token_out));
            }
        }
        None
    }

    /// Compute `amountOutMin` from `debt_to_cover`, the flash-loan premium, and slippage.
    ///
    /// The Executor swap leg converts seized collateral back to the debt asset.
    /// This minimum output must be high enough that the swap covers the flash-loan repayment
    /// (`debt_to_cover + premium`) after slippage.
    ///
    /// Formula: `debt_to_cover * (basis + premium_bps - slippage_bps) / basis`
    /// with `basis = 10_000` and Aave V3 default `premium_bps = 5`.
    fn compute_amount_out_min(debt_to_cover: U256, slippage_max_bps: u32) -> U256 {
        const FLASH_LOAN_PREMIUM_BPS: u32 = 5; // Aave V3 default: 0.05%
        let basis = U256::from(10_000u32);
        let premium = U256::from(FLASH_LOAN_PREMIUM_BPS);
        let slippage = U256::from(slippage_max_bps);
        // amount_out_min = debt_to_cover * (10000 + 5 - slippage) / 10000
        //
        // RiskConfig::validate caps slippage_max_bps at 200, but a
        // hand-constructed RiskConfig can bypass load(). If slippage would
        // underflow the discount, fail closed: drop the slippage discount
        // entirely so amountOutMin still covers full repayment + premium.
        let discount = (basis + premium)
            .checked_sub(slippage)
            .unwrap_or(basis + premium);
        debt_to_cover * discount / basis
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VenueEntry;
    use alloy::primitives::address;

    fn test_routing_config() -> RoutingConfig {
        RoutingConfig {
            primary: "test".into(),
            fallbacks: vec![],
            submission_style: "single_atomic_tx".into(),
            venues: vec![VenueEntry {
                name: "test-dex".into(),
                chain: "base".into(),
                liquidity_usd_min: 50_000,
                venue_type: "dex".into(),
                kyc: false,
                router_compatibility: "v2".into(),
                router_address: "0x1111111111111111111111111111111111111111".into(),
                pairs: vec![TradingPair {
                    token_in: "0x3333333333333333333333333333333333333333".into(),
                    token_out: "0x2222222222222222222222222222222222222222".into(),
                }],
            }],
            forensic_tag_sources: vec![],
        }
    }

    fn test_risk_config() -> RiskConfig {
        RiskConfig::default()
    }

    #[test]
    fn test_resolve_v2_finds_matching_pair() {
        let routing = test_routing_config();
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let debt_to_cover = U256::from(1_000_000_000_000_000_000u128); // 1 ETH

        let route = resolver
            .resolve_v2(collateral, debt, "base", debt_to_cover)
            .expect("should resolve matching pair");

        assert_eq!(
            route.router,
            address!("0x1111111111111111111111111111111111111111")
        );
        assert_eq!(route.path, vec![collateral, debt]);
        assert_eq!(route.venue_name, "test-dex");

        // amount_out_min should be debt_to_cover * (10000 + 5 - 50) / 10000
        // = 1e18 * 9955 / 10000
        let expected_min = debt_to_cover * U256::from(9955) / U256::from(10000);
        assert_eq!(route.amount_out_min, expected_min);
    }

    #[test]
    fn test_resolve_v2_no_match_returns_none() {
        let routing = test_routing_config();
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x4444444444444444444444444444444444444444"); // unknown
        let collateral = address!("0x5555555555555555555555555555555555555555");
        let debt_to_cover = U256::from(1000);

        let result = resolver.resolve_v2(collateral, debt, "base", debt_to_cover);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_v2_skips_non_dex_venues() {
        let routing = RoutingConfig {
            primary: "test".into(),
            fallbacks: vec![],
            submission_style: "single_atomic_tx".into(),
            venues: vec![VenueEntry {
                name: "kyc-venue".into(),
                chain: "base".into(),
                liquidity_usd_min: 50_000,
                venue_type: "aggregator".into(), // not "dex"
                kyc: false,
                router_compatibility: "v2".into(),
                router_address: "0x1111111111111111111111111111111111111111".into(),
                pairs: vec![TradingPair {
                    token_in: "0x3333333333333333333333333333333333333333".into(),
                    token_out: "0x2222222222222222222222222222222222222222".into(),
                }],
            }],
            forensic_tag_sources: vec![],
        };
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let result = resolver.resolve_v2(collateral, debt, "base", U256::from(1000));
        assert!(result.is_none(), "non-dex venue should be skipped");
    }

    #[test]
    fn test_resolve_v2_skips_empty_router() {
        let routing = RoutingConfig {
            primary: "test".into(),
            fallbacks: vec![],
            submission_style: "single_atomic_tx".into(),
            venues: vec![VenueEntry {
                name: "no-router-dex".into(),
                chain: "base".into(),
                liquidity_usd_min: 50_000,
                venue_type: "dex".into(),
                kyc: false,
                router_compatibility: "v2".into(),
                router_address: String::new(), // empty
                pairs: vec![TradingPair {
                    token_in: "0x3333333333333333333333333333333333333333".into(),
                    token_out: "0x2222222222222222222222222222222222222222".into(),
                }],
            }],
            forensic_tag_sources: vec![],
        };
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let result = resolver.resolve_v2(collateral, debt, "base", U256::from(1000));
        assert!(
            result.is_none(),
            "venue with empty router should be skipped"
        );
    }

    #[test]
    fn test_compute_amount_out_min_slippage() {
        // With 50 bps (0.5%) slippage + 5 bps premium, amount_out_min = 9955/10000 of debt_to_cover
        let debt = U256::from(1_000_000u64);
        let result = RoutingResolver::compute_amount_out_min(debt, 50);
        assert_eq!(result, U256::from(995_500u64));

        // With 0 bps slippage, amount_out_min = debt_to_cover * (10000 + 5) / 10000
        let result = RoutingResolver::compute_amount_out_min(debt, 0);
        assert_eq!(result, U256::from(1_000_500u64));

        // With 100 bps (1%) slippage, amount_out_min = 9905/10000 of debt
        let result = RoutingResolver::compute_amount_out_min(debt, 100);
        assert_eq!(result, U256::from(990_500u64));
    }

    #[test]
    fn test_compute_amount_out_min_with_premium() {
        // Verify that the minimum accounts for flash-loan premium
        let debt = U256::from(1_000_000_000_000_000_000u128); // 1 ETH (18 decimals)
        let result = RoutingResolver::compute_amount_out_min(debt, 50);
        // With 50 bps slippage and 5 bps premium: 1e18 * (10000 + 5 - 50) / 10000 = 1e18 * 0.9955
        let expected = U256::from(995_500_000_000_000_000u128);
        assert_eq!(
            result, expected,
            "amountOutMin should include premium adjustment"
        );
    }

    #[test]
    fn test_compute_amount_out_min_mixed_decimals() {
        // USDC (6 decimals) debt against WETH (18 decimals) collateral
        // 1000 USDC = 1_000_000_000 atomic units
        let debt = U256::from(1_000_000_000u128); // 1000 USDC (6 decimals)
        let result = RoutingResolver::compute_amount_out_min(debt, 50);
        // 1e9 * (10000 + 5 - 50) / 10000 = 1e9 * 0.9955 = 995_500_000
        let expected = U256::from(995_500_000u128);
        assert_eq!(result, expected, "6-decimal USDC should scale correctly");
        assert!(
            result < debt,
            "slippage should reduce amountOutMin from debt_to_cover"
        );
    }

    #[test]
    fn test_routing_config_field_deserialize() {
        // Verify the route uses real config values from disk when available.
        let risk_path = std::path::Path::new("../config/risk.yaml");
        let risk_path = if risk_path.exists() {
            risk_path
        } else {
            std::path::Path::new("config/risk.yaml")
        };
        if risk_path.exists() {
            let risk = RiskConfig::load(risk_path).expect("risk.yaml should parse");
            // Verify slippage_max_bps is the configured 50 bps default
            assert_eq!(risk.slippage_max_bps, 50);
        }
    }

    /// Two v2 venues on the same chain, both matching the pair.
    fn two_venue_routing_config() -> RoutingConfig {
        let pair = TradingPair {
            token_in: "0x3333333333333333333333333333333333333333".into(),
            token_out: "0x2222222222222222222222222222222222222222".into(),
        };
        RoutingConfig {
            primary: "test".into(),
            fallbacks: vec![],
            submission_style: "single_atomic_tx".into(),
            venues: vec![
                VenueEntry {
                    name: "venue-a".into(),
                    chain: "base".into(),
                    liquidity_usd_min: 50_000,
                    venue_type: "dex".into(),
                    kyc: false,
                    router_compatibility: "v2".into(),
                    router_address: "0x1111111111111111111111111111111111111111".into(),
                    pairs: vec![pair.clone()],
                },
                VenueEntry {
                    name: "venue-b".into(),
                    chain: "base".into(),
                    liquidity_usd_min: 50_000,
                    venue_type: "dex".into(),
                    kyc: false,
                    router_compatibility: "v2".into(),
                    router_address: "0x4444444444444444444444444444444444444444".into(),
                    pairs: vec![pair],
                },
            ],
            forensic_tag_sources: vec![],
        }
    }

    #[test]
    fn test_resolve_v2_eligible_falls_through_to_next_eligible_venue() {
        let routing = two_venue_routing_config();
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let debt_to_cover = U256::from(1_000_000u64);

        // venue-a is rotation-blocked: resolution must fall through to venue-b
        // instead of returning venue-a (which pacing would deny → candidate drop).
        let recent = vec!["venue-a".to_string()];
        let route = resolver
            .resolve_v2_eligible(collateral, debt, "base", debt_to_cover, |venue| {
                !recent.iter().any(|r| r == venue)
            })
            .expect("should fall through to the next eligible venue");

        assert_eq!(route.venue_name, "venue-b");
        assert_eq!(
            route.router,
            address!("0x4444444444444444444444444444444444444444")
        );
        // amount_out_min semantics preserved: same slippage formula as resolve_v2.
        let expected_min = debt_to_cover * U256::from(9955) / U256::from(10000);
        assert_eq!(route.amount_out_min, expected_min);
    }

    #[test]
    fn test_resolve_v2_eligible_all_blocked_returns_none() {
        let routing = two_venue_routing_config();
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");

        let result =
            resolver.resolve_v2_eligible(collateral, debt, "base", U256::from(1000), |_| false);
        assert!(result.is_none(), "all venues blocked must resolve to None");
    }

    #[test]
    fn test_resolve_v2_eligible_all_eligible_matches_resolve_v2() {
        let routing = two_venue_routing_config();
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let debt_to_cover = U256::from(1_000_000u64);

        let via_default = resolver.resolve_v2(collateral, debt, "base", debt_to_cover);
        let via_eligible =
            resolver.resolve_v2_eligible(collateral, debt, "base", debt_to_cover, |_| true);
        assert_eq!(via_default, via_eligible);
        assert_eq!(via_default.unwrap().venue_name, "venue-a");
    }

    #[test]
    fn test_compute_amount_out_min_absurd_slippage_fails_closed() {
        // Slippage above basis+premium would underflow the discount term.
        // The guard must fail closed: no slippage discount at all, i.e.
        // amountOutMin = debt * (10000 + 5) / 10000 — never a tiny/zero minimum.
        let debt = U256::from(1_000_000u64);
        let result = RoutingResolver::compute_amount_out_min(debt, 20_000);
        assert_eq!(result, U256::from(1_000_500u64));
    }

    #[test]
    fn test_resolve_v2_skips_v3_venues() {
        let routing = RoutingConfig {
            primary: "test".into(),
            fallbacks: vec![],
            submission_style: "single_atomic_tx".into(),
            venues: vec![VenueEntry {
                name: "uniswap-v3".into(),
                chain: "base".into(),
                liquidity_usd_min: 50_000,
                venue_type: "dex".into(),
                router_compatibility: "v3".into(),
                kyc: false,
                router_address: "0x1111111111111111111111111111111111111111".into(),
                pairs: vec![TradingPair {
                    token_in: "0x3333333333333333333333333333333333333333".into(),
                    token_out: "0x2222222222222222222222222222222222222222".into(),
                }],
            }],
            forensic_tag_sources: vec![],
        };
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let result = resolver.resolve_v2(collateral, debt, "base", U256::from(1000));
        assert!(result.is_none(), "V3 venue should be skipped");
    }

    #[test]
    fn test_resolve_v2_skips_custom_venues() {
        let routing = RoutingConfig {
            primary: "test".into(),
            fallbacks: vec![],
            submission_style: "single_atomic_tx".into(),
            venues: vec![VenueEntry {
                name: "custom-dex".into(),
                chain: "base".into(),
                liquidity_usd_min: 50_000,
                venue_type: "dex".into(),
                router_compatibility: "custom".into(),
                kyc: false,
                router_address: "0x1111111111111111111111111111111111111111".into(),
                pairs: vec![TradingPair {
                    token_in: "0x3333333333333333333333333333333333333333".into(),
                    token_out: "0x2222222222222222222222222222222222222222".into(),
                }],
            }],
            forensic_tag_sources: vec![],
        };
        let risk = test_risk_config();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let result = resolver.resolve_v2(collateral, debt, "base", U256::from(1000));
        assert!(result.is_none(), "custom venue should be skipped");
    }
}
