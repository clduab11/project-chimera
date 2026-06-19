// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/**
 * @title IAavePool
 * @notice Minimal interface for Aave V3 Pool operations required by
 *         the Chimera flash-loan executor.
 *
 * @dev Covers:
 *   - flashLoanSimple  : initiates a single-asset flash loan
 *   - liquidationCall  : liquidate an undercollateralized position
 *   - events           : FlashLoan (emitted by Pool), LiquidationCall
 */
interface IAavePool {
    /**
     * @notice Request a flash loan of a single asset.
     * @param receiverAddress The contract receiving the flash loan (must implement IFlashLoanSimpleReceiver)
     * @param asset           The token to flash-borrow
     * @param amount          Amount to borrow
     * @param params          Arbitrary bytes passed through to executeOperation callback
     * @param referralCode    Referral code for tracking (0 for none)
     */
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;

    /**
     * @notice Liquidate an undercollateralized position.
     * @param collateralAsset The collateral asset to seize
     * @param debtAsset       The debt asset to repay
     * @param user            The user being liquidated
     * @param debtToCover     Amount of debt to cover (use type(uint256).max for full)
     * @param receiveAToken   true = receive aTokens; false = receive underlying collateral
     */
    function liquidationCall(
        address collateralAsset,
        address debtAsset,
        address user,
        uint256 debtToCover,
        bool receiveAToken
    ) external;

    // ── Events ──

    /**
     * @notice Emitted when a flash loan is initiated.
     */
    event FlashLoan(
        address indexed target,
        address indexed initiator,
        address indexed asset,
        uint256 amount,
        uint256 premium,
        uint16 referralCode
    );

    /**
     * @notice Emitted when a liquidation occurs.
     */
    event LiquidationCall(
        address indexed collateralAsset,
        address indexed debtAsset,
        address indexed user,
        uint256 debtToCover,
        uint256 liquidatedCollateralAmount,
        address liquidator,
        bool receiveAToken
    );
}
