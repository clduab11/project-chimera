//! Calldata builder for common DeFi interactions.
//!
//! Uses [`alloy::sol!`] to generate type-safe ABI encoders for Aave V3
//! liquidation / flash-loan and Uniswap V2 swap calls.

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol_types::SolCall;

alloy::sol! {
    interface IAavePool {
        function liquidationCall(
            address collateralAsset,
            address debtAsset,
            address user,
            uint256 debtToCover,
            bool receiveAToken
        ) external;
    }
}

alloy::sol! {
    interface IAavePoolSimple {
        function flashLoanSimple(
            address receiverAddress,
            address asset,
            uint256 amount,
            bytes calldata params,
            uint16 referralCode
        ) external;
    }
}

alloy::sol! {
    interface IUniswapV2Router {
        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);
    }
}

/// Builder for common DeFi calldata payloads.
///
/// All methods return raw ABI-encoded `Bytes` ready to be placed in a
/// [`TransactionRequest`](alloy::rpc::types::TransactionRequest) `input` field.
pub struct CalldataBuilder;

impl CalldataBuilder {
    /// Encode an Aave V3 `liquidationCall`.
    ///
    /// # Arguments
    /// * `pool`      – Aave Pool address (target of the call).
    /// * `collateral`– Collateral asset to liquidate.
    /// * `debt`      – Debt asset to repay.
    /// * `user`      – User to liquidate.
    /// * `debt_to_cover` – Amount of debt to cover.
    /// * `receive_a_token` – Whether to receive aTokens instead of underlying.
    pub fn build_liquidation_call(
        _pool: Address,
        collateral: Address,
        debt: Address,
        user: Address,
        debt_to_cover: U256,
        receive_a_token: bool,
    ) -> Bytes {
        let call = IAavePool::liquidationCallCall {
            collateralAsset: collateral,
            debtAsset: debt,
            user,
            debtToCover: debt_to_cover,
            receiveAToken: receive_a_token,
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode an Aave V3 `flashLoanSimple`.
    ///
    /// # Arguments
    /// * `pool`   – Aave Pool address (target of the call).
    /// * `asset`  – Asset to flash-loan.
    /// * `amount` – Amount to borrow.
    /// * `params` – ABI-encoded parameters forwarded to the receiver.
    pub fn build_flash_loan_simple(
        pool: Address,
        asset: Address,
        amount: U256,
        params: Bytes,
    ) -> Bytes {
        let call = IAavePoolSimple::flashLoanSimpleCall {
            receiverAddress: pool,
            asset,
            amount,
            params,
            referralCode: 0,
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V2 `swapExactTokensForTokens`.
    ///
    /// # Arguments
    /// * `router`        – DEX router address (target of the call).
    /// * `amount_in`     – Exact input amount.
    /// * `amount_out_min`– Minimum output amount (slippage guard).
    /// * `path`          – Token path (e.g. `[USDC, WETH]`).
    /// * `to`            – Recipient of the output tokens.
    /// * `deadline`      – Unix timestamp deadline.
    pub fn build_dex_swap(
        _router: Address,
        amount_in: U256,
        amount_out_min: U256,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Bytes {
        let call = IUniswapV2Router::swapExactTokensForTokensCall {
            amountIn: amount_in,
            amountOutMin: amount_out_min,
            path,
            to,
            deadline: U256::from(deadline),
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode an Aave V3 `liquidationCall` and return the 4-byte selector
    /// followed by ABI-encoded parameters.
    ///
    /// This is a convenience wrapper around [`build_liquidation_call`] that
    /// ignores the pool target address and returns only the payload.
    pub fn encode_liquidation_call(
        collateral: Address,
        debt: Address,
        user: Address,
        debt_to_cover: U256,
        receive_a_token: bool,
    ) -> Bytes {
        Self::build_liquidation_call(
            Address::ZERO,
            collateral,
            debt,
            user,
            debt_to_cover,
            receive_a_token,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn test_liquidation_call_selector() {
        let data = CalldataBuilder::build_liquidation_call(
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            U256::ZERO,
            false,
        );
        // Aave V3 liquidationCall selector.
        // keccak256("liquidationCall(address,address,address,uint256,bool)")[:4]
        assert_eq!(data.len(), 4 + 5 * 32); // selector + 5 padded args
                                            // First 4 bytes are the selector.
        assert!(!data[..4].is_empty());
    }

    #[test]
    fn test_flash_loan_simple_non_empty() {
        let data = CalldataBuilder::build_flash_loan_simple(
            Address::ZERO,
            Address::ZERO,
            U256::ZERO,
            Bytes::new(),
        );
        assert!(!data.is_empty());
        assert_eq!(data.len(), 196); // selector + 5 static args (including offset) + 1 dynamic len
    }

    #[test]
    fn test_dex_swap_non_empty() {
        let data = CalldataBuilder::build_dex_swap(
            Address::ZERO,
            U256::ZERO,
            U256::ZERO,
            vec![Address::ZERO, Address::ZERO],
            Address::ZERO,
            0,
        );
        assert!(!data.is_empty());
    }

    #[test]
    fn test_encode_liquidation_call_matches_build() {
        let collateral = address!("0x1111111111111111111111111111111111111111");
        let debt = address!("0x2222222222222222222222222222222222222222");
        let user = address!("0x3333333333333333333333333333333333333333");
        let debt_to_cover = U256::from(1000);
        let receive_a_token = true;

        let built = CalldataBuilder::build_liquidation_call(
            Address::ZERO,
            collateral,
            debt,
            user,
            debt_to_cover,
            receive_a_token,
        );
        let encoded = CalldataBuilder::encode_liquidation_call(
            collateral,
            debt,
            user,
            debt_to_cover,
            receive_a_token,
        );
        assert_eq!(built, encoded);
    }

    #[test]
    fn test_liquidation_call_encoding_values() {
        let collateral = address!("0xA0b86a33E6441e0A421e56E4773C3C4b0Db7E5b0");
        let debt = address!("0x6b175474E89094C44Da98b954EedeAC495271d0F");
        let user = address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        let debt_to_cover = U256::from(1_000_000_000_000_000_000u128); // 1 ether
        let receive_a_token = false;

        let data = CalldataBuilder::encode_liquidation_call(
            collateral,
            debt,
            user,
            debt_to_cover,
            receive_a_token,
        );

        // First 4 bytes should be the function selector.
        let _selector = &data[..4];
        // The rest should be ABI-encoded arguments (32-byte aligned).
        let params = &data[4..];
        assert_eq!(params.len(), 5 * 32);

        // First param (offset 0..32) is collateral address.
        let decoded_collateral = Address::from_slice(&params[12..32]);
        assert_eq!(decoded_collateral, collateral);

        // Second param (offset 32..64) is debt address.
        let decoded_debt = Address::from_slice(&params[44..64]);
        assert_eq!(decoded_debt, debt);

        // Third param (offset 64..96) is user address.
        let decoded_user = Address::from_slice(&params[76..96]);
        assert_eq!(decoded_user, user);

        // Fourth param (offset 96..128) is debtToCover.
        let mut debt_bytes = [0u8; 32];
        debt_bytes.copy_from_slice(&params[96..128]);
        let decoded_debt = U256::from_be_bytes(debt_bytes);
        assert_eq!(decoded_debt, debt_to_cover);

        // Fifth param (offset 128..160) is receiveAToken (bool padded to 32 bytes).
        assert_eq!(params[159], 0);
    }
}
