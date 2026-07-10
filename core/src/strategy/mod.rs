//! Strategy assembly module for Chimera Core.
//!
//! Builds atomic liquidation requests for the standalone deployed Executor.
//! Consumes resolved V2 routes and assembles structurally exact
//! `execute(bytes)` calldata.

pub mod assembler;

pub use assembler::StrategyAssembler;
