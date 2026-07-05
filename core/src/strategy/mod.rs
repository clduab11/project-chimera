//! Strategy assembly module for Chimera Core.
//!
//! Builds flash-loan atomic liquidation transactions using the
//! worker-as-Executor pattern. Consumes resolved V2 routes and
//! assembles structurally-exact `flashLoanSimple` calldata.

pub mod assembler;

pub use assembler::StrategyAssembler;
