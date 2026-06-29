object "Executor" {
    // GöÇGöÇ Constructor GöÇGöÇ
    // Copies runtime bytecode to memory and returns it.
    code {
        datacopy(0, dataoffset("runtime"), datasize("runtime"))
        return(0, datasize("runtime"))
    }
    object "runtime" {
        code {
            // GòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉGòÉ
            // SECTION 0: CONSTANT SELECTORS & SIGNATURES
            // Aave V3 IFlashLoanSimpleReceiver.executeOperation
            // Signature: executeOperation(address,uint256,uint256,address,bytes)
            // Computes first 4 bytes of keccak256 hash of the above.
            // Used by Aave Pool to callback into this contract.
            let SEL_EXECUTE_OPERATION := 0x1b11d0ff
            // Direct execution entry for EIP-7702 / manual trigger.
            // Signature: exec(bytes)
            let SEL_EXEC := 0x55f86501
            // Aave V3 Pool.liquidationCall
            // Signature: liquidationCall(address,address,address,uint256,bool)
            let SEL_LIQUIDATION_CALL := 0x00a718a9
            // Uniswap V2 / compatible DEX: swapExactTokensForTokens
            // Signature: swapExactTokensForTokens(uint256,uint256,address[],address,uint256)
            let SEL_SWAP_EXACT_TOKENS := 0x38ed1739
            // ERC20 standard selectors
            let SEL_BALANCE_OF := 0x70a08231 // balanceOf(address)
            let SEL_APPROVE      := 0x095ea7b3 // approve(address,uint256)
            let SEL_TRANSFER     := 0xa9059cbb // transfer(address,uint256)
            // Event: Profit(uint256 amount)  (non-indexed parameter)
            // topic0 = keccak256("Profit(uint256)")
            let EVT_PROFIT_TOPIC0 := 0x357d905f1831209797df4d55d79c5c5bf1d9f7311c976afd05e13d881eab9bc8
            // Custom error selectors (4-byte signatures)
            // Used for clean revert reasons compatible with Solidity try/catch.
            let ERR_PROFIT_GATE   := 0x2e5a0d02 // ProfitGateFailed()
            let ERR_ATOMIC_FAIL   := 0x5fe2e75c // AtomicFail()
            let ERR_UNAUTHORIZED  := 0x82b42900 // Unauthorized()
            let ERR_INVALID_ROUTER := 0x8d4f59a9 // InvalidDexRouter()
            // SECTION 1: MAIN CALLDATA DISPATCHER
            // Extract function selector: highest 4 bytes of calldata.
            let sig := shr(224, calldataload(0))
            switch sig
            // GöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇGöÇ
            // CASE A: Aave V3 Flash Loan Callback (executeOperation)
            case 0x1b11d0ff { // SEL_EXECUTE_OPERATION
                // Calldata layout (ABI-encoded by Aave Pool):
                //   [0x00:0x04)  function selector
                //   [0x04:0x24)  asset            (address, padded to 32)
                //   [0x24:0x44)  amount           (uint256)
                //   [0x44:0x64)  premium          (uint256)
                //   [0x64:0x84)  initiator        (address, padded to 32)
                //   [0x84:0xA4)  params.offset    (uint256, relative to 0x04)
                //   [0xA4:0xC4)  params.length    (uint256, at offset=params.offset+0x04)
                //   [0xC4:...)   params data      (StrategyParams tightly packed)
                // GöÇGöÇ Decode fixed arguments GöÇGöÇ
                let asset     := calldataload(4)
                let amount    := calldataload(36)
                let premium   := calldataload(68)
                let initiator := calldataload(100)
                // Decode dynamic bytes (params)
                let paramsOffset    := add(calldataload(132), 4)
                let paramsLen       := calldataload(paramsOffset)
                let paramsDataStart := add(paramsOffset, 32)
                // GöÇGöÇ Validate params length GöÇGöÇ
                // StrategyParams: 9 * 32 = 288 bytes.
                if lt(paramsLen, 288) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // GöÇGöÇ Decode StrategyParams GöÇGöÇ
                //   [0x00:0x20)  collateralAsset   (address, left-padded)
                //   [0x20:0x40)  userToLiquidate   (address, left-padded)
                //   [0x40:0x60)  debtToCover       (uint256)
                //   [0x60:0x80)  receiveAToken       (uint256, 0 or 1)
                //   [0x80:0xA0)  dexRouter           (address, left-padded)
                //   [0xA0:0xC0)  amountOutMin      (uint256)
                //   [0xC0:0xE0)  minProfit           (uint256)
                //   [0xE0:0x100) tip                 (uint256)
                //   [0x100:0x120) deadline           (uint256)
                let collateralAsset := calldataload(paramsDataStart)
                let userToLiquidate := calldataload(add(paramsDataStart, 32))
                let debtToCover     := calldataload(add(paramsDataStart, 64))
                let receiveAToken   := calldataload(add(paramsDataStart, 96))
                let dexRouter       := calldataload(add(paramsDataStart, 128))
                let amountOutMin    := calldataload(add(paramsDataStart, 160))
                let minProfit       := calldataload(add(paramsDataStart, 192))
                let tip             := calldataload(add(paramsDataStart, 224))
                let deadline        := calldataload(add(paramsDataStart, 256))
                // GöÇGöÇ Record pre-flight balance of debt token GöÇGöÇ
                let self := address()
                let balanceBefore := callBalanceOf(asset, self)
                // GöÇGöÇ Step 1: LIQUIDATION GöÇGöÇ
                // Approve Aave Pool to pull debtToCover of the debt asset.
                let pool := caller()
                if iszero(callApprove(asset, pool, debtToCover)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // Call liquidationCall on Aave Pool.
                // This seizes collateral from the target user.
                if iszero(callLiquidation(pool, collateralAsset, asset, userToLiquidate, debtToCover, receiveAToken)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // GöÇGöÇ Step 2: DEX SWAP GöÇGöÇ
                // Swap seized collateral back into the debt token.
                // Skip if collateral is already the debt token (rare but possible).
                if iszero(eq(collateralAsset, asset)) {
                    if iszero(dexRouter) {
                        revertWithError(ERR_INVALID_ROUTER)
                    }
                    // Query seized collateral balance.
                    let collateralBal := callBalanceOf(collateralAsset, self)
                    // Approve DEX router to spend seized collateral.
                    if iszero(callApprove(collateralAsset, dexRouter, collateralBal)) {
                        revertWithError(ERR_ATOMIC_FAIL)
                    }
                    // Execute swap: collateralAsset -> asset (debt token).
                    if iszero(callSwapExactTokens(dexRouter, collateralBal, amountOutMin, collateralAsset, asset, self, deadline)) {
                        revertWithError(ERR_ATOMIC_FAIL)
                    }
                }
                // GöÇGöÇ Step 3: REPAY FLASH LOAN GöÇGöÇ
                // Approve Aave Pool to pull back flash-loaned amount + premium.
                let repayAmt := add(amount, premium)
                if iszero(callApprove(asset, pool, repayAmt)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // GöÇGöÇ Step 4: PROFIT GATE GöÇGöÇ
                // Ensure the strategy was profitable after covering all costs.
                // Profit check: balanceAfter > balanceBefore + minProfit + tip.
                // minProfit should be set off-chain to cover gas, slippage, etc.
                let balanceAfter := callBalanceOf(asset, self)
                let requiredBalance := add(add(balanceBefore, minProfit), tip)
                if iszero(gt(balanceAfter, requiredBalance)) {
                    revertWithError(ERR_PROFIT_GATE)
                }
                // GöÇGöÇ Step 5: EMIT PROFIT EVENT GöÇGöÇ
                let profit := sub(balanceAfter, balanceBefore)
                mstore(0, profit)
                log1(0, 32, EVT_PROFIT_TOPIC0)
                // GöÇGöÇ Step 6: RETURN TRUE TO AAVE GöÇGöÇ
                // Aave Pool expects a bool return value.
                mstore(0, 1)
                return(0, 32)
            }
            // CASE B: Direct Execution Entry (EIP-7702 compatible)
            case 0x55f86501 { // SEL_EXEC
                // This path allows an EOA (with EIP-7702 code delegation)
                // or an external controller to execute a strategy directly
                // without going through Aave's flash-loan callback.
                // The caller must have already arranged token balances.
                //
                // Calldata layout:
                //   [0x00:0x04)  selector
                //   [0x04:0x24)  strategyData.offset  (relative to 0x04)
                //   [0x24:0x44)  strategyData.length
                //   [0x44:...)   strategyData
                // strategyData tightly packed (12 * 32 = 384 bytes):
                //   [0x00:0x20)  asset
                //   [0x20:0x40)  amount
                //   [0x40:0x60)  pool
                //   [0x60:0x80)  collateralAsset
                //   [0x80:0xA0)  userToLiquidate
                //   [0xA0:0xC0)  debtToCover
                //   [0xC0:0xE0)  receiveAToken
                //   [0xE0:0x100) dexRouter
                //   [0x100:0x120) amountOutMin
                //   [0x120:0x140) minProfit
                //   [0x140:0x160) tip
                //   [0x160:0x180) deadline
                let dataOffset    := add(calldataload(4), 4)
                let dataLen       := calldataload(dataOffset)
                let dataStart     := add(dataOffset, 32)
                if lt(dataLen, 384) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                let d_asset       := calldataload(dataStart)
                let d_amount      := calldataload(add(dataStart, 32))
                let d_pool        := calldataload(add(dataStart, 64))
                let d_collateral  := calldataload(add(dataStart, 96))
                let d_user        := calldataload(add(dataStart, 128))
                let d_debtToCover := calldataload(add(dataStart, 160))
                let d_receiveAToken := calldataload(add(dataStart, 192))
                let d_dexRouter   := calldataload(add(dataStart, 224))
                let d_amountOutMin := calldataload(add(dataStart, 256))
                let d_minProfit   := calldataload(add(dataStart, 288))
                let d_tip         := calldataload(add(dataStart, 320))
                let d_deadline    := calldataload(add(dataStart, 352))
                let self := address()
                let balanceBefore := callBalanceOf(d_asset, self)
                // GöÇGöÇ Liquidation GöÇGöÇ
                if iszero(callApprove(d_asset, d_pool, d_debtToCover)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                if iszero(callLiquidation(d_pool, d_collateral, d_asset, d_user, d_debtToCover, d_receiveAToken)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // GöÇGöÇ Swap GöÇGöÇ
                if iszero(eq(d_collateral, d_asset)) {
                    if iszero(d_dexRouter) {
                        revertWithError(ERR_INVALID_ROUTER)
                    }
                    let colBal := callBalanceOf(d_collateral, self)
                    if iszero(callApprove(d_collateral, d_dexRouter, colBal)) {
                        revertWithError(ERR_ATOMIC_FAIL)
                    }
                    if iszero(callSwapExactTokens(d_dexRouter, colBal, d_amountOutMin, d_collateral, d_asset, self, d_deadline)) {
                        revertWithError(ERR_ATOMIC_FAIL)
                    }
                }
                // GöÇGöÇ Profit Gate GöÇGöÇ
                let balanceAfter := callBalanceOf(d_asset, self)
                let required := add(add(balanceBefore, d_minProfit), d_tip)
                if iszero(gt(balanceAfter, required)) {
                    revertWithError(ERR_PROFIT_GATE)
                }
                // GöÇGöÇ Emit Profit Event GöÇGöÇ
                let profit := sub(balanceAfter, balanceBefore)
                mstore(0, profit)
                log1(0, 32, EVT_PROFIT_TOPIC0)
                stop()
            }
            // DEFAULT: Reject unknown selectors & plain ETH transfers
            default {
                revertWithError(ERR_UNAUTHORIZED)
            }
            // SECTION 2: INTERNAL HELPER FUNCTIONS
            // GöÇGöÇ callBalanceOf GöÇGöÇ
            // Queries ERC20 balanceOf for a given token and account.
            // Uses staticcall (read-only). Reverts on failure.
            //
            // @param token   Token contract address
            // @param account Address to query balance for
            // @return bal    Token balance (uint256)
            function callBalanceOf(token, account) -> bal {
                // Encode: balanceOf(address)
                mstore(0, shl(224, 0x70a08231)) // SEL_BALANCE_OF
                mstore(4, account)
                // staticcall: gas(), token, inOffset=0, inSize=36, outOffset=0, outSize=32
                if iszero(staticcall(gas(), token, 0, 36, 0, 32)) {
                    revert(0, 0)
                }
                bal := mload(0)
            }
            // GöÇGöÇ callApprove GöÇGöÇ
            // Calls ERC20 approve. Handles tokens that return nothing (e.g. USDT)
            // or return bool. Returns true only if the call succeeded AND
            // the return data (if any) is true.
            // @param spender Address to approve
            // @param amount  Allowance amount
            // @return success true if approval succeeded
            function callApprove(token, spender, amount) -> success {
                mstore(0, shl(224, 0x095ea7b3)) // SEL_APPROVE
                mstore(4, spender)
                mstore(36, amount)
                success := call(gas(), token, 0, 0, 68, 0, 32)
                // Some ERC20s (USDT) don't return a bool. If returndatasize == 0,
                // assume success if the call itself succeeded.
                if success {
                    if returndatasize() {
                        returndatacopy(0, 0, 32)
                        if iszero(mload(0)) {
                            success := 0
                        }
                    }
                }
            }
            // GöÇGöÇ callLiquidation GöÇGöÇ
            // Calls Aave V3 Pool.liquidationCall.
            // Reverts on this level are handled by the caller checking `success`.
            // @param pool            Aave Pool address
            // @param collateralAsset Asset to seize
            // @param debtAsset       Debt to repay
            // @param user            User being liquidated
            // @param debtToCover     Amount of debt to cover
            // @param receiveAToken   true = receive aTokens, false = receive underlying
            // @return success        true if liquidationCall succeeded
            function callLiquidation(pool, collateralAsset, debtAsset, user, debtToCover, receiveAToken) -> success {
                // liquidationCall(address,address,address,uint256,bool)
                mstore(0, shl(224, 0x00a718a9)) // SEL_LIQUIDATION_CALL
                mstore(4, collateralAsset)
                mstore(36, debtAsset)
                mstore(68, user)
                mstore(100, debtToCover)
                mstore(132, receiveAToken) // bool encoded as uint256 (0 or 1)
                success := call(gas(), pool, 0, 0, 164, 0, 0)
            }
            // GöÇGöÇ callSwapExactTokens GöÇGöÇ
            // Calls Uniswap V2 compatible swapExactTokensForTokens with a 2-hop path.
            // Builds the ABI-encoded calldata in scratch memory and executes the call.
            // @param router       DEX router address
            // @param amountIn     Collateral amount to swap
            // @param amountOutMin Minimum output (slippage protection)
            // @param tokenIn      Input token (collateral)
            // @param tokenOut     Output token (debt token)
            // @param to           Recipient of output tokens
            // @param deadline     Transaction deadline timestamp
            // @return success     true if swap succeeded
            function callSwapExactTokens(router, amountIn, amountOutMin, tokenIn, tokenOut, to, deadline) -> success {
                // Memory layout for swapExactTokensForTokens calldata:
                //   [0x04:0x24)  amountIn
                //   [0x24:0x44)  amountOutMin
                //   [0x44:0x64)  path offset (relative to start of args = 0x04)
                //   [0x64:0x84)  to
                //   [0x84:0xA4)  deadline
                //   [0xA4:0xC4)  path.length (= 2)
                //   [0xC4:0xE4)  path[0] (tokenIn)
                //   [0xE4:0x104) path[1] (tokenOut)
                // path offset = 0xA0 = 160 (bytes from 0x04 to 0xA4)
                // GöÇ Write fixed parameters GöÇ
                mstore(0, shl(224, 0x38ed1739)) // SEL_SWAP_EXACT_TOKENS
                mstore(4, amountIn)
                mstore(36, amountOutMin)
                mstore(68, 160)          // path offset
                mstore(100, to)
                mstore(132, deadline)
                // GöÇ Write dynamic path array GöÇ
                mstore(164, 2)           // path.length
                mstore(196, tokenIn)
                mstore(228, tokenOut)
                // Total calldata size: 260 bytes (0x104)
                success := call(gas(), router, 0, 0, 260, 0, 0)
            }
            // GöÇGöÇ revertWithError GöÇGöÇ
            // Reverts with a 4-byte custom error selector.
            // Compatible with Solidity custom errors and Foundry testing.
            // @param selector 4-byte error signature
            function revertWithError(selector) {
                mstore(0, shl(224, selector))
                revert(0, 4)
            }
        }
    }
}
