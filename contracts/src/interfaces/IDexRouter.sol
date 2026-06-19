// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/**
 * @title IDexRouter
 * @notice Generic DEX router interface for token-to-token swaps.
 * @dev Designed to be compatible with Uniswap V2 clones and similar routers.
 *      Extending to V3 or other DEXes only requires updating the Executor's
 *      selector-based routing logic in Yul.
 */
interface IDexRouter {
    /**
     * @notice Swap an exact amount of input tokens for as many output tokens as possible.
     * @param amountIn      The amount of input tokens to send
     * @param amountOutMin  The minimum output tokens (slippage protection)
     * @param path          An array of token addresses (path[0] = input, path[last] = output)
     * @param to            Recipient of output tokens
     * @param deadline      Unix timestamp after which the transaction reverts
     * @return amounts      An array of amounts exchanged per hop
     */
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);

    /**
     * @notice Given an input amount and a path, return the maximum output amounts.
     * @param amountIn The amount of input tokens
     * @param path     An array of token addresses
     * @return amounts An array of amounts out per hop
     */
    function getAmountsOut(
        uint256 amountIn,
        address[] calldata path
    ) external view returns (uint256[] memory amounts);
}
