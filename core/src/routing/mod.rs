//! Route resolution module for Chimera Core.
//!
//! Resolves collateral/debt pairs to DEX venues and computes
//! slippage-adjusted minimum output amounts for swap legs.

pub mod resolver;

pub use resolver::{ResolvedV2Route, RoutingResolver, NO_SWAP_VENUE};
