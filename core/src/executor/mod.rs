//! Transaction execution module for Chimera Core.
//!
//! Provides the [`TransactionExecutor`] trait, [`BuiltTransaction`] and
//! [`SubmissionReceipt`] types, plus concrete implementations for calldata
//! construction and RPC submission.

pub mod balance;
pub mod builder;
pub mod submitter;

pub use builder::CalldataBuilder;
pub use submitter::RpcSubmitter;

use crate::ChimeraError;
use alloy::primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;

/// A fully-built EVM transaction ready for signing and broadcast.
#[derive(Debug, Clone)]
pub struct BuiltTransaction {
    /// Target contract address.
    pub to: Address,
    /// ABI-encoded call data.
    pub data: Bytes,
    /// ETH value to send.
    pub value: U256,
    /// Gas limit (units).
    pub gas_limit: u64,
    /// Maximum fee per gas (EIP-1559).
    pub max_fee_per_gas: u128,
    /// Maximum priority fee per gas (EIP-1559).
    pub max_priority_fee_per_gas: u128,
    /// Transaction nonce.
    pub nonce: u64,
}

/// Receipt returned after a transaction has been mined (or failed).
#[derive(Debug, Clone)]
pub struct SubmissionReceipt {
    /// Transaction hash.
    pub tx_hash: B256,
    /// Block number in which the tx was included, if confirmed.
    pub block_number: Option<u64>,
    /// Gas consumed, if known.
    pub gas_used: Option<u64>,
    /// `true` if the transaction succeeded on-chain.
    pub status: bool,
}

/// Trait for transaction submission backends.
///
/// Implementors are responsible for gas estimation, signing, broadcast and
/// receipt polling.
#[async_trait]
pub trait TransactionExecutor: Send + Sync {
    /// Submit a built transaction to the network and return its hash.
    async fn submit(&self, tx: BuiltTransaction) -> Result<B256, ChimeraError>;

    /// Estimate the gas required for a built transaction.
    async fn estimate_gas(&self, tx: &BuiltTransaction) -> Result<u64, ChimeraError>;

    /// Return the chain ID this executor is bound to.
    fn chain_id(&self) -> u64;
}
