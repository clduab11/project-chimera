//! Pre-warming logic for the REVM CacheDB.
//! This module handles loading state from `snapshot_generator.py` output into the simulator's
//! in-memory database to avoid RPC calls during hot-path simulation.

use crate::ChimeraError;
use alloy::primitives::{keccak256, Address, B256, U256};
use revm::database::CacheDB;
use serde::Deserialize;
use std::collections::HashMap;

/// Represents the JSON structure output by `snapshot_generator.py`.
#[derive(Deserialize, Debug)]
pub struct MarketSnapshot {
    pub chain: String,
    pub block_number: u64,
    /// Aave V3 Pool address. Defaults to ZERO if not present in JSON.
    #[serde(default)]
    pub pool: Address,
    pub reserves: Vec<ReserveData>,
    pub users: HashMap<Address, UserPosition>,
}

#[derive(Deserialize, Debug)]
pub struct ReserveData {
    pub symbol: String,
    pub address: Address,
    pub decimals: u8,
    pub price_usd: f64,
    pub ltv: u16,
    pub liquidation_threshold: u16,
    pub liquidation_bonus: u16,
    pub liquidity_rate: u128,
    pub variable_borrow_rate: u128,
    pub total_variable_debt: u128,
    /// aToken address for this reserve. Defaults to ZERO if not present in JSON.
    #[serde(default)]
    pub a_token: Address,
    /// Variable debt token address for this reserve. Defaults to ZERO if not present in JSON.
    #[serde(default)]
    pub variable_debt_token: Address,
}

/// Pack reserve configuration parameters into Aave V3's `ReserveConfigurationMap` bitmap.
///
/// Bit layout (Aave V3 Pool v1 `ReserveConfigurationMap`):
/// - 0-15    : LTV
/// - 16-31   : liquidation threshold
/// - 32-47   : liquidation bonus
/// - 48-55   : decimals
/// - 56      : active
/// - 57      : frozen
/// - 58      : borrowingEnabled
/// - 59      : stableRateBorrowingEnabled
/// - 60      : paused
/// - 61      : borrowableInIsolationMode
/// - 62      : siloedBorrowing
/// - 63      : flashLoanEnabled
pub fn pack_reserve_configuration_map(reserve: &ReserveData) -> U256 {
    let mut word = U256::from(reserve.ltv)
        + (U256::from(reserve.liquidation_threshold) << 16)
        + (U256::from(reserve.liquidation_bonus) << 32)
        + (U256::from(reserve.decimals) << 48);
    word |= U256::from(1) << 56; // active
    word |= U256::from(1) << 58; // borrowingEnabled
    word |= U256::from(1) << 63; // flashLoanEnabled
    word
}

#[derive(Deserialize, Debug, Default)]
pub struct UserPosition {
    pub collateral: HashMap<Address, U256>,
    pub debt: HashMap<Address, U256>,
    pub emode_category: u8,
}

/// Converts a 20-byte [`Address`] to a 32-byte [`B256`] by left-padding with zeros.
/// This matches Solidity's `abi.encode(address)` behaviour.
fn address_to_b256(addr: Address) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(addr.as_slice());
    B256::from(bytes)
}

/// Calculates the storage slot for a Solidity mapping entry.
///
/// Aave V3 (and standard Solidity) uses the following layout for a mapping:
/// `slot = keccak256(abi.encodePacked(key, mapping_slot))`
///
/// Where:
/// - `key` is left-padded to 32 bytes (as a [`B256`])
/// - `mapping_slot` is encoded as 32 bytes big-endian
///
/// The `contract` parameter is included for API clarity; the slot calculation is
/// contract-agnostic because each contract has its own independent storage trie in REVM.
///
/// For ERC20 `balanceOf` mappings, `mapping_slot` is typically `0` since `_balances`
/// is the first state variable in the standard OpenZeppelin layout inherited by Aave tokens.
///
/// # References
/// - Aave V3 Token contracts: <https://github.com/aave/aave-v3-core/tree/master/contracts/protocol/tokenization>
/// - Solidity storage layout: <https://docs.soliditylang.org/en/latest/internals/layout_in_storage.html>
pub fn calculate_storage_slot(_contract: Address, mapping_slot: U256, key: B256) -> U256 {
    // Concatenate key (32 bytes) + mapping_slot (32 bytes, big-endian)
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(key.as_slice());
    data[32..].copy_from_slice(&mapping_slot.to_be_bytes::<32>());

    let hash = keccak256(data);
    U256::from_be_bytes(hash.0)
}

/// Calculates the base storage slot for a reserve's `ReserveData` struct inside the Aave V3 Pool.
///
/// The Pool contract stores reserves in a mapping:
/// `mapping(address => ReserveData) internal _reserves;`
///
/// The base slot for a specific reserve is:
/// `keccak256(reserve_address (32 bytes) + mapping_slot (32 bytes))`
///
/// In Aave V3 Pool v1, the `_reserves` mapping is at slot `53`.
///
/// Individual fields within `ReserveData` are then accessed at fixed offsets from this base:
/// - Offset 0: `ReserveConfigurationMap configuration` (packed bitmap, 256 bits)
/// - Offset 1: `liquidityIndex` (128 bits) + `variableBorrowIndex` (128 bits)
/// - Offset 2: `currentLiquidityRate` (128 bits) + `currentVariableBorrowRate` (128 bits)
/// - Offset 3: `currentStableBorrowRate` (128 bits) + `lastUpdateTimestamp` (40 bits) + `id` (16 bits)
///
/// # References
/// - Aave V3 PoolStorage: <https://github.com/aave/aave-v3-core/blob/master/contracts/protocol/pool/PoolStorage.sol>
pub fn calculate_reserve_data_slot(pool: Address, reserve: Address, mapping_slot: U256) -> U256 {
    let reserve_word = address_to_b256(reserve);
    calculate_storage_slot(pool, mapping_slot, reserve_word)
}

/// Pre-warms the database with reserve and user state.
/// Generic over the inner DB type so it works with both InMemoryDB and AlloyDB.
pub fn pre_warm_db<ExtDB: revm::database_interface::DatabaseRef>(
    db: &mut CacheDB<ExtDB>,
    snapshot: &MarketSnapshot,
) -> Result<(), ChimeraError> {
    println!(
        "[simulator] Pre-warming DB with snapshot from block {}...",
        snapshot.block_number
    );

    for reserve in &snapshot.reserves {
        // Skip reserves where token addresses are not populated (backward compatibility).
        let has_a_token = !reserve.a_token.is_zero();
        let has_debt_token = !reserve.variable_debt_token.is_zero();

        // Pre-warm aToken and variableDebtToken balances for each user.
        for (user, position) in &snapshot.users {
            // aToken balances (collateral)
            if has_a_token {
                if let Some(balance) = position.collateral.get(&reserve.address) {
                    let user_word = address_to_b256(*user);
                    let slot = calculate_storage_slot(reserve.a_token, U256::ZERO, user_word);
                    let _ = db.insert_account_storage(reserve.a_token, slot, *balance);
                }
            }

            // Variable debt token balances
            if has_debt_token {
                if let Some(balance) = position.debt.get(&reserve.address) {
                    let user_word = address_to_b256(*user);
                    let slot =
                        calculate_storage_slot(reserve.variable_debt_token, U256::ZERO, user_word);
                    let _ = db.insert_account_storage(reserve.variable_debt_token, slot, *balance);
                }
            }
        }

        // Pre-warm reserve data in the Pool contract.
        // Aave V3 Pool stores reserves in a mapping at slot 53.
        if !snapshot.pool.is_zero() {
            let base = calculate_reserve_data_slot(snapshot.pool, reserve.address, U256::from(53));

            // Offset 0: ReserveConfigurationMap (packed configuration bitmap).
            let config_word = pack_reserve_configuration_map(reserve);
            let _ = db.insert_account_storage(snapshot.pool, base, config_word);

            // Offset 1: liquidityIndex (128 bits) + variableBorrowIndex (128 bits).
            // Use RAY (1e27) as neutral indices for mock snapshots.
            let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128);
            let indices = (ray << 128) | ray;
            let _ = db.insert_account_storage(snapshot.pool, base + U256::from(1), indices);

            // Offset 2: currentLiquidityRate (128 bits) + currentVariableBorrowRate (128 bits).
            let rates = (U256::from(reserve.liquidity_rate) << 128)
                | U256::from(reserve.variable_borrow_rate);
            let _ = db.insert_account_storage(snapshot.pool, base + U256::from(2), rates);

            // Offset 3: currentStableBorrowRate (128 bits) + lastUpdateTimestamp (40 bits) + id (16 bits).
            let _ = db.insert_account_storage(snapshot.pool, base + U256::from(3), U256::ZERO);
        }

        // Legacy price warming (kept for backward compatibility).
        let price_slot = keccak256(reserve.address.as_slice());
        let _ = db.insert_account_storage(
            Address::ZERO,
            price_slot.into(),
            U256::from((reserve.price_usd * 1e8) as u128),
        );
        println!("[simulator] Pre-warmed price for {}", reserve.symbol);
    }

    println!("[simulator] Pre-warming complete.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use revm::database::EmptyDB;

    #[test]
    fn test_calculate_storage_slot_zero_vector() {
        // Test vector: all zeros.
        // contract = 0x00..00, mapping_slot = 0, key = 0x00..00
        // Expected: keccak256([0u8; 64])
        let slot = calculate_storage_slot(Address::ZERO, U256::ZERO, B256::ZERO);
        let expected_hash = keccak256([0u8; 64]);
        let expected = U256::from_be_bytes(expected_hash.0);
        assert_eq!(slot, expected);
    }

    #[test]
    fn test_calculate_storage_slot_with_address() {
        // Test with a non-zero user address and mapping slot 0.
        let user = address!("0x1234567890123456789012345678901234567890");
        let contract = address!("0xA0b86a33E6441e0A421e56E4773C3C4b0Db7E5b0");
        let mapping_slot = U256::ZERO;

        let user_word = address_to_b256(user);
        let slot = calculate_storage_slot(contract, mapping_slot, user_word);

        // Manually compute expected value.
        let mut data = [0u8; 64];
        data[..32].copy_from_slice(user_word.as_slice());
        data[32..].copy_from_slice(&mapping_slot.to_be_bytes::<32>());
        let expected = U256::from_be_bytes(keccak256(data).0);

        assert_eq!(slot, expected);
    }

    #[test]
    fn test_calculate_storage_slot_mapping_slot_one() {
        // Test with mapping_slot = 1 (e.g. _allowances in some ERC20 layouts).
        let user = address!("0x1234567890123456789012345678901234567890");
        let contract = address!("0xA0b86a33E6441e0A421e56E4773C3C4b0Db7E5b0");
        let mapping_slot = U256::from(1);

        let user_word = address_to_b256(user);
        let slot = calculate_storage_slot(contract, mapping_slot, user_word);

        let mut data = [0u8; 64];
        data[..32].copy_from_slice(user_word.as_slice());
        data[32..].copy_from_slice(&mapping_slot.to_be_bytes::<32>());
        let expected = U256::from_be_bytes(keccak256(data).0);

        assert_eq!(slot, expected);
    }

    #[test]
    fn test_calculate_reserve_data_slot_determinism() {
        let pool = address!("0x794a61358D6845594F94dc1DB02A252b5b4814aD"); // Aave V3 Pool on Arbitrum
        let reserve = address!("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"); // WETH
        let mapping_slot = U256::from(53);

        let slot = calculate_reserve_data_slot(pool, reserve, mapping_slot);

        // Must be non-zero and deterministic.
        assert!(!slot.is_zero());
        let slot2 = calculate_reserve_data_slot(pool, reserve, mapping_slot);
        assert_eq!(slot, slot2);
    }

    #[test]
    fn test_pre_warm_db_inserts_expected_slots() {
        let mut db = CacheDB::new(EmptyDB::default());

        let reserve = ReserveData {
            symbol: "WETH".to_string(),
            address: address!("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"),
            decimals: 18,
            price_usd: 2500.0,
            ltv: 8000,
            liquidation_threshold: 8250,
            liquidation_bonus: 10500,
            liquidity_rate: 3_000_000_000_000_000_000_000_000_000u128,
            variable_borrow_rate: 5_000_000_000_000_000_000_000_000_000u128,
            total_variable_debt: 700_000_000_000_000_000_000_000_000_000u128,
            a_token: address!("0xe50fA9b3c56FfB159cB0FCA61F5c9D750e8128c8"),
            variable_debt_token: address!("0x0c84331e39d6658Cd6e6b9ba04736cC4c4734351"),
        };

        let user = address!("0x1234567890123456789012345678901234567890");
        let mut collateral = HashMap::new();
        collateral.insert(reserve.address, U256::from(1_000_000_000_000_000_000u128));
        let mut debt = HashMap::new();
        debt.insert(reserve.address, U256::from(500_000_000_000_000_000u128));

        let mut users = HashMap::new();
        users.insert(
            user,
            UserPosition {
                collateral,
                debt,
                emode_category: 0,
            },
        );

        let snapshot = MarketSnapshot {
            chain: "arbitrum".to_string(),
            block_number: 100_000_000,
            pool: address!("0x794a61358D6845594F94dc1DB02A252b5b4814aD"),
            reserves: vec![reserve],
            users,
        };

        pre_warm_db(&mut db, &snapshot).unwrap();

        // Verify expected accounts were created in CacheDB.
        // For each reserve we expect:
        //   - aToken account with 1 balance slot
        //   - variableDebtToken account with 1 balance slot
        //   - Pool account with 4 reserve data slots + 1 price slot (at Address::ZERO)
        // Total distinct contract accounts touched: aToken, debtToken, Pool, Address::ZERO

        let a_token_account = db.cache.accounts.get(&snapshot.reserves[0].a_token);
        assert!(
            a_token_account.is_some(),
            "aToken account should be pre-warmed"
        );
        let a_token_storage = &a_token_account.unwrap().storage;
        assert_eq!(
            a_token_storage.len(),
            1,
            "aToken should have exactly 1 balance slot"
        );

        let debt_account = db
            .cache
            .accounts
            .get(&snapshot.reserves[0].variable_debt_token);
        assert!(
            debt_account.is_some(),
            "variableDebtToken account should be pre-warmed"
        );
        let debt_storage = &debt_account.unwrap().storage;
        assert_eq!(
            debt_storage.len(),
            1,
            "variableDebtToken should have exactly 1 balance slot"
        );

        let pool_account = db.cache.accounts.get(&snapshot.pool);
        assert!(pool_account.is_some(), "Pool account should be pre-warmed");
        let pool_storage = &pool_account.unwrap().storage;
        assert_eq!(
            pool_storage.len(),
            4,
            "Pool should have 4 reserve data slots (config, indices, rates, timestamp+id)"
        );

        let price_account = db.cache.accounts.get(&Address::ZERO);
        assert!(
            price_account.is_some(),
            "Price oracle placeholder account should be pre-warmed"
        );
    }

    #[test]
    fn test_pre_warm_db_skips_zero_tokens() {
        // Ensure we don't crash when a_token / variable_debt_token are zero (old JSON).
        let mut db = CacheDB::new(EmptyDB::default());

        let reserve = ReserveData {
            symbol: "UNKNOWN".to_string(),
            address: address!("0x1111111111111111111111111111111111111111"),
            decimals: 18,
            price_usd: 100.0,
            ltv: 0,
            liquidation_threshold: 0,
            liquidation_bonus: 0,
            liquidity_rate: 0,
            variable_borrow_rate: 0,
            total_variable_debt: 0,
            a_token: Address::ZERO,
            variable_debt_token: Address::ZERO,
        };
        let _a_token_addr = reserve.a_token;
        let _debt_token_addr = reserve.variable_debt_token;

        let user = address!("0x1234567890123456789012345678901234567890");
        let mut collateral = HashMap::new();
        collateral.insert(reserve.address, U256::from(1_000u128));

        let mut users = HashMap::new();
        users.insert(
            user,
            UserPosition {
                collateral,
                debt: HashMap::new(),
                emode_category: 0,
            },
        );

        let snapshot = MarketSnapshot {
            chain: "arbitrum".to_string(),
            block_number: 100_000_000,
            pool: Address::ZERO, // pool not known
            reserves: vec![reserve],
            users,
        };

        // Should complete without error and not insert any token slots.
        pre_warm_db(&mut db, &snapshot).unwrap();

        let user_word = user.into_word();
        let a_token_slot = calculate_storage_slot(Address::ZERO, U256::from(52), user_word);
        let debt_token_slot = calculate_storage_slot(Address::ZERO, U256::from(44), user_word);

        if let Some(account) = db.cache.accounts.get(&Address::ZERO) {
            assert!(
                !account.storage.contains_key(&a_token_slot),
                "aToken storage slot should not be created"
            );
            assert!(
                !account.storage.contains_key(&debt_token_slot),
                "debtToken storage slot should not be created"
            );
        }
        // Price slot is still inserted at Address::ZERO for backward compatibility.
        assert!(
            db.cache.accounts.contains_key(&Address::ZERO),
            "Price slot should still be inserted at Address::ZERO"
        );
    }
}
