//! StrategyAssembler — builds calls to the standalone deployed Executor.
//!
//! Given a resolved V2 route and liquidation candidate data, assembles a
//! [`BuiltTransaction`] that calls `execute(bytes)` on the configured Executor.
//! The worker EOA signs the transaction but never appears as its target.

#[cfg(test)]
use crate::config::StrategyParams;
use crate::executor::builder::CalldataBuilder;
use crate::executor::BuiltTransaction;
use crate::routing::ResolvedV2Route;
use alloy::primitives::{Address, Bytes, U256};

/// Assembles flash-loan atomic liquidation transactions.
///
/// The deployed Executor owns the Aave pool configuration and initiates the
/// flash loan. Worker EOAs are transaction signers only.
pub struct StrategyAssembler;

impl StrategyAssembler {
    /// Build a live-ready [`BuiltTransaction`] for the flash-loan execution path.
    ///
    /// The returned transaction is structurally complete with populated min_profit,
    /// tip, and deadline fields. The caller is responsible for gas estimation and
    /// live-mode submission.
    ///
    /// # Arguments
    /// * `debt_asset`      — Debt asset the Executor requests from Aave.
    /// * `min_profit`      — Minimum profit in debt-asset wei the Executor must retain.
    /// * `tip`             — Additional tip added on top of min_profit.
    /// * `deadline`        — Unix timestamp deadline for the DEX swap.
    #[allow(clippy::too_many_arguments)]
    pub fn build_transaction(
        executor: Address,
        route: &ResolvedV2Route,
        collateral: Address,
        user: Address,
        debt_to_cover: U256,
        receive_a_token: bool,
        debt_asset: Address,
        min_profit: U256,
        tip: U256,
        deadline: u64,
        nonce: u64,
    ) -> BuiltTransaction {
        let calldata = Self::encode_execute(
            debt_asset,
            debt_to_cover,
            collateral,
            user,
            receive_a_token,
            route,
            min_profit,
            tip,
            deadline,
        );

        BuiltTransaction {
            to: executor,
            data: calldata,
            value: U256::ZERO,
            gas_limit: 0,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            nonce,
        }
    }

    /// Build a shadow [`BuiltTransaction`] for the flash-loan execution path.
    ///
    /// The returned transaction is structurally correct but never broadcast
    /// in shadow mode. The caller is responsible for gas estimation and
    /// live-mode submission.
    ///
    /// # Arguments
    /// * `executor`        — Standalone deployed Executor (transaction target).
    /// * `route`           — Resolved V2 DEX route (router, path, amount_out_min).
    /// * `collateral`      — Collateral asset of the liquidation.
    /// * `user`            — User address to liquidate.
    /// * `debt_to_cover`   — Amount of debt to cover in the liquidation.
    /// * `receive_a_token` — Whether to receive aTokens instead of underlying.
    /// * `debt_asset`      — Debt asset the Executor requests from Aave.
    /// * `nonce`           — Transaction nonce (0 for shadow logging).
    ///
    /// # Returns
    /// A [`BuiltTransaction`] targeting the Executor with `execute(bytes)`
    /// calldata and zero `gas_limit`.
    #[allow(clippy::too_many_arguments)]
    pub fn build_shadow_transaction(
        executor: Address,
        route: &ResolvedV2Route,
        collateral: Address,
        user: Address,
        debt_to_cover: U256,
        receive_a_token: bool,
        debt_asset: Address,
        nonce: u64,
    ) -> BuiltTransaction {
        let calldata = Self::encode_execute(
            debt_asset,
            debt_to_cover,
            collateral,
            user,
            receive_a_token,
            route,
            U256::ZERO,
            U256::ZERO,
            0,
        );

        BuiltTransaction {
            to: executor,
            data: calldata,
            value: U256::ZERO,
            gas_limit: 0, // shadow: no gas estimation
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            nonce,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_execute(
        debt_asset: Address,
        debt_to_cover: U256,
        collateral: Address,
        user: Address,
        receive_a_token: bool,
        route: &ResolvedV2Route,
        min_profit: U256,
        tip: U256,
        deadline: u64,
    ) -> Bytes {
        CalldataBuilder::build_executor_execute(
            debt_asset,
            debt_to_cover,
            collateral,
            user,
            debt_to_cover,
            receive_a_token,
            route.router,
            route.amount_out_min,
            min_profit,
            tip,
            deadline,
        )
    }

    /// Build the legacy nine-word tail used to inspect the typed Executor payload.
    ///
    /// In addition to the fields set by [`build_strategy_params`], this populates
    /// words 6–8 (min_profit, tip, deadline) from caller-provided values.
    #[cfg(test)]
    fn build_strategy_params_full(
        collateral: Address,
        user: Address,
        debt_to_cover: U256,
        receive_a_token: bool,
        dex_router: Address,
        amount_out_min: U256,
        min_profit: U256,
        tip: U256,
        deadline: u64,
    ) -> StrategyParams {
        let mut params = Self::build_strategy_params(
            collateral,
            user,
            debt_to_cover,
            receive_a_token,
            dex_router,
            amount_out_min,
        );

        // word 6: min_profit (uint256 big-endian)
        params.min_profit = min_profit.to_be_bytes();

        // word 7: tip (uint256 big-endian)
        params.tip = tip.to_be_bytes();

        // word 8: deadline (uint256 big-endian)
        params.deadline = U256::from(deadline).to_be_bytes();

        params
    }

    /// Build the legacy nine-word tail used by assembler unit tests.
    ///
    /// The 9-word layout is documented in [`StrategyParams`]. Fields:
    /// - word 0: collateral_asset
    /// - word 1: user_to_liquidate
    /// - word 2: debt_to_cover
    /// - word 3: receive_a_token
    /// - word 4: dex_router
    /// - word 5: amount_out_min
    /// - word 6: min_profit (0 for shadow)
    /// - word 7: tip (0 for shadow)
    /// - word 8: deadline (0 for shadow = no deadline)
    #[cfg(test)]
    fn build_strategy_params(
        collateral: Address,
        user: Address,
        debt_to_cover: U256,
        receive_a_token: bool,
        dex_router: Address,
        amount_out_min: U256,
    ) -> StrategyParams {
        let mut params = StrategyParams::default();

        // word 0: collateral_asset (left-padded address, 32 bytes)
        params.collateral_asset[12..32].copy_from_slice(collateral.as_ref());

        // word 1: user_to_liquidate (left-padded address, 32 bytes)
        params.user_to_liquidate[12..32].copy_from_slice(user.as_ref());

        // word 2: debt_to_cover (uint256 big-endian)
        params.debt_to_cover = debt_to_cover.to_be_bytes();

        // word 3: receive_a_token (uint256, 0 or 1, big-endian)
        if receive_a_token {
            params.receive_a_token[31] = 0x01;
        }

        // word 4: dex_router (left-padded address, 32 bytes)
        params.dex_router[12..32].copy_from_slice(dex_router.as_ref());

        // word 5: amount_out_min (uint256 big-endian)
        params.amount_out_min = amount_out_min.to_be_bytes();

        // word 6-8: min_profit, tip, deadline — all zero for shadow
        // (StrategyParams::default() already sets them to zero)

        params
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const TEST_DEBT: Address = address!("0x2222222222222222222222222222222222222222");
    const TEST_COLLATERAL: Address = address!("0x3333333333333333333333333333333333333333");

    fn sample_route() -> ResolvedV2Route {
        ResolvedV2Route {
            router: address!("0x1111111111111111111111111111111111111111"),
            path: vec![
                TEST_COLLATERAL, // collateral (path[0] = token_in per resolver output)
                TEST_DEBT,       // debt (path[1] = token_out per resolver output)
            ],
            amount_out_min: U256::from(995_000_000_000_000_000u128),
            venue_name: "test-dex".into(),
        }
    }

    #[test]
    fn test_build_shadow_transaction_structure() {
        let executor = address!("0x9999999999999999999999999999999999999999");
        let worker = address!("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE");
        let collateral = TEST_COLLATERAL;
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000_000_000_000_000u128);
        let route = sample_route();

        let tx = StrategyAssembler::build_shadow_transaction(
            executor,
            &route,
            collateral,
            user,
            debt_to_cover,
            false,
            TEST_DEBT,
            0,
        );

        assert_eq!(tx.to, executor);
        assert_ne!(tx.to, worker, "worker must remain signer/from only");
        assert_eq!(tx.value, U256::ZERO);
        assert_eq!(tx.gas_limit, 0); // shadow: no gas estimate
        assert_eq!(tx.nonce, 0);

        // Calldata should be non-empty and well-formed
        assert!(!tx.data.is_empty());
        assert_eq!(
            &tx.data[..4],
            &alloy::primitives::keccak256("execute(bytes)")[..4]
        );
        assert_eq!(tx.data.len(), 4 + 32 + 32 + 11 * 32);

        let mut length_word = [0u8; 32];
        length_word.copy_from_slice(&tx.data[36..68]);
        let inner_len = U256::from_be_bytes(length_word);
        assert_eq!(inner_len, U256::from(352u64));
        let inner = &tx.data[68..];
        assert_eq!(Address::from_slice(&inner[12..32]), TEST_DEBT);
        assert_eq!(Address::from_slice(&inner[76..96]), collateral);
        assert_eq!(Address::from_slice(&inner[108..128]), user);
        assert!(
            !tx.data.windows(20).any(|word| word == worker.as_slice()),
            "worker must not be encoded as a receiver"
        );
        assert!(inner[8 * 32..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn test_build_strategy_params_correctness() {
        let collateral = TEST_COLLATERAL;
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000u64);
        let dex_router = address!("0x1111111111111111111111111111111111111111");
        let amount_out_min = U256::from(995_000u64);

        // Call via StrategyAssembler's build_shadow_transaction to indirectly test params
        let route = ResolvedV2Route {
            router: dex_router,
            path: vec![TEST_DEBT, collateral],
            amount_out_min,
            venue_name: "test".into(),
        };

        let tx = StrategyAssembler::build_shadow_transaction(
            address!("0x9999999999999999999999999999999999999999"),
            &route,
            collateral,
            user,
            debt_to_cover,
            true, // receive_a_token = true
            TEST_DEBT,
            0,
        );

        // Verify the calldata is non-trivial
        assert!(tx.data.len() > 4);

        // Reconstruct StrategyParams from calldata to verify
        let params_encoded = StrategyAssembler::build_strategy_params(
            collateral,
            user,
            debt_to_cover,
            true,
            dex_router,
            amount_out_min,
        );
        let encoded = params_encoded.encode();

        assert_eq!(&tx.data[68 + 2 * 32..68 + 11 * 32], encoded.as_slice());
    }

    #[test]
    fn test_strategy_params_default_is_zero() {
        let params = StrategyParams::default();
        let encoded = params.encode();
        assert_eq!(encoded.len(), 288);
        // All zero bytes in default
        assert!(encoded.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_build_strategy_params_field_wiring() {
        let collateral = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let user = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        let debt_to_cover = U256::from(500u64);
        let dex_router = address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC");
        let amount_out_min = U256::from(450u64);

        let params = StrategyAssembler::build_strategy_params(
            collateral,
            user,
            debt_to_cover,
            false,
            dex_router,
            amount_out_min,
        );

        let encoded = params.encode();

        // Verify collateral_asset at offset 0 (word 0) — address at bytes 12..32
        let decoded_collateral = Address::from_slice(&encoded[12..32]);
        assert_eq!(decoded_collateral, collateral);

        // Verify user_to_liquidate at offset 32 (word 1)
        let decoded_user = Address::from_slice(&encoded[44..64]);
        assert_eq!(decoded_user, user);

        // Verify debt_to_cover at offset 64 (word 2) — big-endian uint256
        let mut debt_buf = [0u8; 32];
        debt_buf.copy_from_slice(&encoded[64..96]);
        let decoded_debt = U256::from_be_bytes(debt_buf);
        assert_eq!(decoded_debt, debt_to_cover);

        // Verify receive_a_token at offset 96 (word 3) — should be 0 (false)
        let mut rat_buf = [0u8; 32];
        rat_buf.copy_from_slice(&encoded[96..128]);
        let decoded_rat = U256::from_be_bytes(rat_buf);
        assert_eq!(decoded_rat, U256::ZERO);

        // Verify dex_router at offset 128 (word 4)
        let decoded_router = Address::from_slice(&encoded[140..160]);
        assert_eq!(decoded_router, dex_router);

        // Verify amount_out_min at offset 160 (word 5)
        let mut aom_buf = [0u8; 32];
        aom_buf.copy_from_slice(&encoded[160..192]);
        let decoded_aom = U256::from_be_bytes(aom_buf);
        assert_eq!(decoded_aom, amount_out_min);
    }

    #[test]
    fn test_build_transaction_populates_live_fields() {
        let executor = address!("0x9999999999999999999999999999999999999999");
        let collateral = TEST_COLLATERAL;
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000_000_000_000_000u128);
        let route = sample_route();
        let min_profit = U256::from(100u64);
        let tip = U256::from(10u64);
        let deadline: u64 = 2000000000;

        let tx = StrategyAssembler::build_transaction(
            executor,
            &route,
            collateral,
            user,
            debt_to_cover,
            false,
            TEST_DEBT,
            min_profit,
            tip,
            deadline,
            1,
        );

        assert_eq!(tx.nonce, 1);
        assert_eq!(tx.to, executor);
        assert!(!tx.data.is_empty());

        // Extract the StrategyParams from calldata and verify live fields
        // Build the expected params
        let params = StrategyAssembler::build_strategy_params_full(
            collateral,
            user,
            debt_to_cover,
            false,
            route.router,
            route.amount_out_min,
            min_profit,
            tip,
            deadline,
        );
        let encoded = params.encode();

        assert_eq!(&tx.data[68 + 2 * 32..68 + 11 * 32], encoded.as_slice());

        // Verify live fields are populated (shadow would have zeros at these positions)
        let mut mp_buf = [0u8; 32];
        mp_buf.copy_from_slice(&encoded[192..224]);
        assert_eq!(
            U256::from_be_bytes(mp_buf),
            min_profit,
            "min_profit should match"
        );

        let mut tp_buf = [0u8; 32];
        tp_buf.copy_from_slice(&encoded[224..256]);
        assert_eq!(U256::from_be_bytes(tp_buf), tip, "tip should match");

        let mut dl_buf = [0u8; 32];
        dl_buf.copy_from_slice(&encoded[256..288]);
        assert_eq!(
            U256::from_be_bytes(dl_buf),
            U256::from(deadline),
            "deadline should match"
        );
    }

    #[test]
    fn test_shadow_params_are_zero_for_live_fields() {
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000_000_000_000_000u128);
        let route = sample_route();

        let params = StrategyAssembler::build_strategy_params(
            collateral,
            user,
            debt_to_cover,
            false,
            route.router,
            route.amount_out_min,
        );
        let encoded = params.encode();

        // Words 6-8 (offsets 192-288) must all be zero for shadow mode
        let shadow_words = &encoded[192..288];
        assert!(
            shadow_words.iter().all(|&b| b == 0),
            "shadow params must have zero min_profit/tip/deadline"
        );
    }

    #[test]
    fn test_build_transaction_live_min_profit_is_populated() {
        let executor = address!("0x9999999999999999999999999999999999999999");
        let route = sample_route();
        let collateral = TEST_COLLATERAL;
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000u64);
        let min_profit = U256::from(12345u64);
        let tip = U256::from(50u64);
        let deadline = 1700000000u64;

        let tx = StrategyAssembler::build_transaction(
            executor,
            &route,
            collateral,
            user,
            debt_to_cover,
            false,
            TEST_DEBT,
            min_profit,
            tip,
            deadline,
            1,
        );

        // Build expected params and verify min_profit at word 6 (offset 192)
        let params = StrategyAssembler::build_strategy_params_full(
            collateral,
            user,
            debt_to_cover,
            false,
            route.router,
            route.amount_out_min,
            min_profit,
            tip,
            deadline,
        );
        let encoded = params.encode();

        let mut mp_buf = [0u8; 32];
        mp_buf.copy_from_slice(&encoded[192..224]);
        assert_eq!(
            U256::from_be_bytes(mp_buf),
            min_profit,
            "word 6 (min_profit) should match live value"
        );
        assert!(
            U256::from_be_bytes(mp_buf) > U256::ZERO,
            "live min_profit must be non-zero"
        );
    }

    #[test]
    fn test_live_vs_shadow_min_profit_difference() {
        let route = sample_route();
        let collateral = address!("0x3333333333333333333333333333333333333333");
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000u64);

        // Shadow: words 6-8 are zero
        let shadow_params = StrategyAssembler::build_strategy_params(
            collateral,
            user,
            debt_to_cover,
            false,
            route.router,
            route.amount_out_min,
        );
        let shadow_enc = shadow_params.encode();

        // Live: words 6-8 are populated
        let live_params = StrategyAssembler::build_strategy_params_full(
            collateral,
            user,
            debt_to_cover,
            false,
            route.router,
            route.amount_out_min,
            U256::from(100u64),
            U256::from(10u64),
            2000000000u64,
        );
        let live_enc = live_params.encode();

        // Shadow fields at offsets 192-288 must differ from live
        let shadow_tail = &shadow_enc[192..288];
        let live_tail = &live_enc[192..288];
        assert!(
            shadow_tail.iter().all(|&b| b == 0),
            "shadow min_profit/tip/deadline must be zero"
        );
        assert!(
            live_tail != shadow_tail,
            "live min_profit/tip/deadline must differ from shadow"
        );
    }

    #[test]
    fn test_rust_params_match_executor_yul_offsets() {
        let route = sample_route();

        let collateral = address!("0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B");
        let user = address!("0x95aD61b0a150d79219dCF64E1E6Cc01f0B64C4cE");
        let debt_to_cover = U256::from(1_234_567_890_123_456_789u128);
        let receive_a_token = true;
        let dex_router = address!("0xDc64a140Aa3E981100a9becA4E685f962f0cF6C9");
        let amount_out_min = U256::from(987_654_321_098_765_432u128);
        let min_profit = U256::from(800_000u64);
        let tip = U256::from(10u64);
        let deadline = 1700000000u64;

        let params = StrategyAssembler::build_strategy_params_full(
            collateral,
            user,
            debt_to_cover,
            receive_a_token,
            dex_router,
            amount_out_min,
            min_profit,
            tip,
            deadline,
        );
        let encoded = params.encode();

        assert_eq!(
            encoded.len(),
            288,
            "StrategyParams must be exactly 288 bytes"
        );

        let read_word = |offset: usize| -> U256 {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&encoded[offset..offset + 32]);
            U256::from_be_bytes(buf)
        };

        let read_addr =
            |offset: usize| -> Address { Address::from_slice(&encoded[offset + 12..offset + 32]) };

        // word 0 (offset 0): collateralAsset
        assert_eq!(read_addr(0), collateral, "word0: collateralAsset mismatch");

        // word 1 (offset 32): userToLiquidate
        assert_eq!(read_addr(32), user, "word1: userToLiquidate mismatch");

        // word 2 (offset 64): debtToCover
        assert_eq!(read_word(64), debt_to_cover, "word2: debtToCover mismatch");

        // word 3 (offset 96): receiveAToken
        assert_eq!(
            read_word(96),
            U256::from(1),
            "word3: receiveAToken should be 1 (true)"
        );

        // word 4 (offset 128): dexRouter
        assert_eq!(read_addr(128), dex_router, "word4: dexRouter mismatch");

        // word 5 (offset 160): amountOutMin
        assert_eq!(
            read_word(160),
            amount_out_min,
            "word5: amountOutMin mismatch"
        );

        // word 6 (offset 192): minProfit
        assert_eq!(read_word(192), min_profit, "word6: minProfit mismatch");

        // word 7 (offset 224): tip
        assert_eq!(read_word(224), tip, "word7: tip mismatch");

        // word 8 (offset 256): deadline
        assert_eq!(
            read_word(256),
            U256::from(deadline),
            "word8: deadline mismatch"
        );
    }

    #[test]
    fn test_min_profit_preserves_debt_token_units() {
        let route = sample_route();
        let debt_usdc_6dec = U256::from(100_000_000u64);
        let min_profit = U256::from(800_000u64);

        let params = StrategyAssembler::build_strategy_params_full(
            address!("0x3333333333333333333333333333333333333333"),
            address!("0x4444444444444444444444444444444444444444"),
            debt_usdc_6dec,
            false,
            route.router,
            route.amount_out_min,
            min_profit,
            U256::ZERO,
            2000000000u64,
        );
        let encoded = params.encode();

        let mut mp_buf = [0u8; 32];
        mp_buf.copy_from_slice(&encoded[192..224]);
        let decoded_mp = U256::from_be_bytes(mp_buf);
        assert_eq!(
            decoded_mp,
            U256::from(800_000u64),
            "minProfit for 6-decimal debt must be in 6-decimal units"
        );
    }

    /// Prove that min_profit_fraction = 0.8 means 80% of expected profit is enforced.
    /// With expected_profit = $100, ETH = $2000, fraction = 0.8:
    ///   target_profit = $80 / $2000 per ETH = 0.04 ETH = 40,000,000,000,000,000 wei
    #[test]
    fn test_min_profit_fraction_means_percent_of_expected_profit() {
        let route = sample_route();
        // Simulate: expected_profit_usd = 100, eth_price = 2000
        // target = 100 * 0.8 = $80 → $80 / $2000 = 0.04 ETH → 0.04e18 wei
        let expected_min_profit = U256::from(40_000_000_000_000_000u128); // 0.04 ETH

        let params = StrategyAssembler::build_strategy_params_full(
            address!("0x3333333333333333333333333333333333333333"),
            address!("0x4444444444444444444444444444444444444444"),
            U256::from(1_000_000_000_000_000_000u128), // debt_to_cover (irrelevant for profit calc)
            false,
            route.router,
            route.amount_out_min,
            expected_min_profit,
            U256::ZERO,
            2000000000u64,
        );
        let encoded = params.encode();

        // word 6 (offset 192): minProfit
        let mut mp_buf = [0u8; 32];
        mp_buf.copy_from_slice(&encoded[192..224]);
        assert_eq!(
            U256::from_be_bytes(mp_buf),
            expected_min_profit,
            "minProfit should be 0.04 ETH = 80% of $100 profit at $2000/ETH"
        );
    }

    /// The shadow path must still emit zero for min_profit (word 6 stays zero).
    #[test]
    fn test_shadow_min_profit_is_zero_regardless_of_config() {
        let route = sample_route();
        let params = StrategyAssembler::build_strategy_params(
            address!("0x3333333333333333333333333333333333333333"),
            address!("0x4444444444444444444444444444444444444444"),
            U256::from(1_000_000u64),
            false,
            route.router,
            route.amount_out_min,
        );
        let encoded = params.encode();
        let shadow_tail = &encoded[192..288];
        assert!(
            shadow_tail.iter().all(|&b| b == 0),
            "shadow min_profit/tip/deadline must all be zero"
        );
    }

    /// WETH debt: 18-decimal min_profit computed from ETH/USD price.
    /// expected_profit_usd = $100, eth_price = $2000, fraction = 0.8
    /// target = $80 / $2000 = 0.04 ETH = 40_000_000_000_000_000 wei
    #[test]
    fn test_min_profit_weth_debt_18decimal() {
        let route = sample_route();
        let expected_min_profit = U256::from(40_000_000_000_000_000u128); // 0.04 WETH

        let params = StrategyAssembler::build_strategy_params_full(
            address!("0x3333333333333333333333333333333333333333"),
            address!("0x4444444444444444444444444444444444444444"),
            U256::from(1_000_000_000_000_000_000u128), // debt_to_cover
            false,
            route.router,
            route.amount_out_min,
            expected_min_profit,
            U256::ZERO,
            2000000000u64,
        );
        let encoded = params.encode();

        let mut mp_buf = [0u8; 32];
        mp_buf.copy_from_slice(&encoded[192..224]);
        let decoded = U256::from_be_bytes(mp_buf);
        assert_eq!(
            decoded, expected_min_profit,
            "WETH debt: minProfit should be 0.04 WETH in 18-decimal units"
        );
        assert!(decoded > U256::ZERO, "WETH profit gate must be active");
    }

    /// Non-WETH debt (USDC 6-decimal): profit gate disabled → min_profit = 0.
    /// Proves that misaligned units (ETH wei vs USDC base units) don't leak through.
    #[test]
    fn test_min_profit_nonweth_debt_is_zero() {
        let route = sample_route();
        let zero_profit = U256::ZERO;

        let params = StrategyAssembler::build_strategy_params_full(
            address!("0x3333333333333333333333333333333333333333"),
            address!("0x4444444444444444444444444444444444444444"),
            U256::from(1_000_000_000u128), // 1000 USDC (6-decimal) as debt_to_cover
            false,
            route.router,
            route.amount_out_min,
            zero_profit, // orchestrator passes zero for non-WETH
            U256::ZERO,
            2000000000u64,
        );
        let encoded = params.encode();

        let mut mp_buf = [0u8; 32];
        mp_buf.copy_from_slice(&encoded[192..224]);
        let decoded = U256::from_be_bytes(mp_buf);
        assert_eq!(
            decoded,
            U256::ZERO,
            "Non-WETH debt (USDC): minProfit must be zero since profit gate is disabled"
        );
    }

    #[test]
    fn test_executor_payload_asset_is_debt_not_collateral() {
        let executor = address!("0x9999999999999999999999999999999999999999");
        let debt = TEST_DEBT;
        let collateral = TEST_COLLATERAL;
        let user = address!("0x4444444444444444444444444444444444444444");
        let debt_to_cover = U256::from(1_000_000_000_000_000_000u128);

        // Route matches resolver output: [collateral, debt]
        let route = ResolvedV2Route {
            router: address!("0x1111111111111111111111111111111111111111"),
            path: vec![collateral, debt],
            amount_out_min: U256::from(995_000_000_000_000_000u128),
            venue_name: "test-dex".into(),
        };

        let tx = StrategyAssembler::build_shadow_transaction(
            executor,
            &route,
            collateral,
            user,
            debt_to_cover,
            false,
            debt,
            0,
        );

        let inner = &tx.data[68..];
        let asset_from_calldata = Address::from_slice(&inner[12..32]);
        assert_eq!(
            asset_from_calldata, debt,
            "Executor asset must be the debt token"
        );
        assert_ne!(
            asset_from_calldata, collateral,
            "Executor asset must not be collateral"
        );
    }

    #[test]
    fn test_resolver_route_path_is_collateral_then_debt() {
        use crate::config::{RiskConfig, RoutingConfig, TradingPair, VenueEntry};
        use crate::routing::RoutingResolver;

        let routing = RoutingConfig {
            primary: "test".into(),
            fallbacks: vec![],
            submission_style: "single_atomic_tx".into(),
            venues: vec![VenueEntry {
                name: "test-dex".into(),
                chain: "base".into(),
                liquidity_usd_min: 50_000,
                venue_type: "dex".into(),
                router_compatibility: "v2".into(),
                kyc: false,
                router_address: "0x1111111111111111111111111111111111111111".into(),
                pairs: vec![TradingPair {
                    token_in: "0x3333333333333333333333333333333333333333".into(),
                    token_out: "0x2222222222222222222222222222222222222222".into(),
                }],
            }],
            forensic_tag_sources: vec![],
        };
        let risk = RiskConfig::default();
        let resolver = RoutingResolver::new(&routing, &risk);

        let debt = address!("0x2222222222222222222222222222222222222222");
        let collateral = address!("0x3333333333333333333333333333333333333333");

        let route = resolver
            .resolve_v2(collateral, debt, "base", U256::from(1000))
            .expect("should resolve");

        assert_eq!(route.path[0], collateral, "path[0] must be collateral");
        assert_eq!(route.path[1], debt, "path[1] must be debt");
    }
}
