object "Executor" {
    // G--G-- Constructor G--G--
    // Copies runtime bytecode to memory and returns it.
    code {
        datacopy(0, dataoffset("runtime"), datasize("runtime"))
        return(0, datasize("runtime"))
    }
    object "runtime" {
        code {
            // G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----G----
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
            // G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--G--
            // CASE A: Aave V3 Flash Loan Callback (executeOperation)
            // ======================================================================
            case 0x1b11d0ff {
                // Calldata layout (ABI-encoded by Aave Pool):
                //   [0x00:0x04)  function selector
                //   [0x04:0x24)  asset            (address, padded to 32)
                //   [0x24:0x44)  amount           (uint256)
                //   [0x44:0x64)  premium          (uint256)
                //   [0x64:0x84)  initiator        (address, padded to 32)
                //   [0x84:0xA4)  params.offset    (uint256, relative to 0x04)
                //   [0xA4:0xC4)  params.length    (uint256, at offset=params.offset+0x04)
                //   [0xC4:...)   params data      (StrategyParams tightly packed)
                // G--G-- Decode fixed arguments G--G--
                let asset     := calldataload(4)
                let amount    := calldataload(36)
                let premium   := calldataload(68)
                let initiator := calldataload(100)

                // -- Decode dynamic bytes (params) --
                let paramsOffset    := add(calldataload(132), 4)
                let paramsLen       := calldataload(paramsOffset)
                let paramsDataStart := add(paramsOffset, 32)
                // G--G-- Validate params length G--G--
                // StrategyParams: 9 * 32 = 288 bytes.
                if lt(paramsLen, 288) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // G--G-- Decode StrategyParams G--G--
                //   [0x00:0x20)  collateralAsset   (address, left-padded)
                //   [0x20:0x40)  userToLiquidate   (address, left-padded)
                //   [0x40:0x60)  debtToCover       (uint256)
                //   [0x60:0x80)  receiveAToken     (uint256, 0 or 1)
                //   [0x80:0xA0)  dexRouter         (address, left-padded)
                //   [0xA0:0xC0)  amountOutMin      (uint256)
                //   [0xC0:0xE0)  minProfit         (uint256)
                //   [0xE0:0x100) tip               (uint256)
                //   [0x100:0x120) deadline          (uint256)
                let collateralAsset := calldataload(paramsDataStart)
                let userToLiquidate := calldataload(add(paramsDataStart, 32))
                let debtToCover     := calldataload(add(paramsDataStart, 64))
                let receiveAToken   := calldataload(add(paramsDataStart, 96))
                let dexRouter       := calldataload(add(paramsDataStart, 128))
                let amountOutMin    := calldataload(add(paramsDataStart, 160))
                let minProfit       := calldataload(add(paramsDataStart, 192))
                let tip             := calldataload(add(paramsDataStart, 224))
                let deadline        := calldataload(add(paramsDataStart, 256))
                // G--G-- Record pre-flight balance of debt token G--G--
                let self := address()
                let balanceBefore := callBalanceOf(asset, self)
                // G--G-- Step 1: LIQUIDATION G--G--
                // Approve Aave Pool to pull debtToCover of the debt asset.
                let pool := caller()
                if iszero(callApprove(asset, pool, debtToCover)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // Call liquidationCall on Aave Pool.
                if iszero(callLiquidation(pool, collateralAsset, asset, userToLiquidate, debtToCover, receiveAToken)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // G--G-- Step 2: DEX SWAP G--G--
                // Swap seized collateral back into the debt token.
                // Skip if collateral is already the debt token.
                if iszero(eq(collateralAsset, asset)) {
                    if iszero(dexRouter) {
                        revertWithError(ERR_INVALID_ROUTER)
                    }
                    let collateralBal := callBalanceOf(collateralAsset, self)
                    if iszero(callApprove(collateralAsset, dexRouter, collateralBal)) {
                        revertWithError(ERR_ATOMIC_FAIL)
                    }
                    if iszero(callSwapExactTokens(dexRouter, collateralBal, amountOutMin, collateralAsset, asset, self, deadline)) {
                        revertWithError(ERR_ATOMIC_FAIL)
                    }
                }
                // G--G-- Step 3: REPAY FLASH LOAN G--G--
                // Approve Aave Pool to pull back flash-loaned amount + premium.
                let repayAmt := add(amount, premium)
                if iszero(callApprove(asset, pool, repayAmt)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // G--G-- Step 4: PROFIT GATE G--G--
                // Ensure the strategy was profitable after covering all costs.
                // Profit check: balanceAfter > balanceBefore + minProfit + tip.
                // minProfit should be set off-chain to cover gas, slippage, etc.
                let balanceAfter := callBalanceOf(asset, self)
                let _sum1 := add(balanceBefore, minProfit)
                if lt(_sum1, balanceBefore) {
                    revertWithError(ERR_PROFIT_GATE)
                }
                let requiredBalance := add(_sum1, tip)
                if lt(requiredBalance, _sum1) {
                    revertWithError(ERR_PROFIT_GATE)
                }
                if iszero(gt(balanceAfter, requiredBalance)) {
                    revertWithError(ERR_PROFIT_GATE)
                }
                // G--G-- Step 5: EMIT PROFIT EVENT G--G--
                let profit := sub(balanceAfter, balanceBefore)
                mstore(0, profit)
                log1(0, 32, EVT_PROFIT_TOPIC0)
                // G--G-- Step 6: RETURN TRUE TO AAVE G--G--
                // Aave Pool expects a bool return value.
                mstore(0, 1)
                return(0, 32)
            }

            // ======================================================================
            // CASE B: Direct Execution Entry (EIP-7702 compatible, owner only)
            // ======================================================================
            case 0x55f86501 {
                // -- Owner auth: only owner can call exec --
                if iszero(eq(caller(), sload(0))) {
                    mstore(0, shl(224, ERR_UNAUTHORIZED))
                    revert(0, 4)
                }

                // Calldata layout:
                //   [0x00:0x04)  selector
                //   [0x04:0x24)  strategyData.offset  (relative to 0x04)
                //   [0x24:0x44)  strategyData.length
                //   [0x44:...)   strategyData
                //
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
                let d_asset         := calldataload(dataStart)
                let d_amount        := calldataload(add(dataStart, 32))
                let d_pool          := calldataload(add(dataStart, 64))
                let d_collateral    := calldataload(add(dataStart, 96))
                let d_user          := calldataload(add(dataStart, 128))
                let d_debtToCover   := calldataload(add(dataStart, 160))
                let d_receiveAToken := calldataload(add(dataStart, 192))
                let d_dexRouter     := calldataload(add(dataStart, 224))
                let d_amountOutMin  := calldataload(add(dataStart, 256))
                let d_minProfit     := calldataload(add(dataStart, 288))
                let d_tip           := calldataload(add(dataStart, 320))
                let d_deadline      := calldataload(add(dataStart, 352))

                let self := address()
                let balanceBefore := callBalanceOf(d_asset, self)
                // G--G-- Liquidation G--G--
                if iszero(callApprove(d_asset, d_pool, d_debtToCover)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                if iszero(callLiquidation(d_pool, d_collateral, d_asset, d_user, d_debtToCover, d_receiveAToken)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // G--G-- Swap G--G--
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
                // G--G-- Profit Gate G--G--
                let balanceAfter := callBalanceOf(d_asset, self)
                let _sum1 := add(balanceBefore, d_minProfit)
                if lt(_sum1, balanceBefore) {
                    revertWithError(ERR_PROFIT_GATE)
                }
                let required := add(_sum1, d_tip)
                if lt(required, _sum1) {
                    revertWithError(ERR_PROFIT_GATE)
                }
                if iszero(gt(balanceAfter, required)) {
                    revertWithError(ERR_PROFIT_GATE)
                }
                // G--G-- Emit Profit Event G--G--
                let profit := sub(balanceAfter, balanceBefore)
                mstore(0, profit)
                log1(0, 32, EVT_PROFIT_TOPIC0)
                stop()
            }

            // -------------------- DEFAULT: reject --------------------
            default {
                mstore(0, shl(224, ERR_UNAUTHORIZED))
                revert(0, 4)
            }
            // SECTION 2: INTERNAL HELPER FUNCTIONS
            // G--G-- callBalanceOf G--G--
            // Queries ERC20 balanceOf for a given token and account.
            // Uses staticcall (read-only). Reverts on failure.
            function callBalanceOf(token, account) -> bal {
                mstore(0, shl(224, 0x70a08231))
                mstore(4, account)
                if iszero(staticcall(gas(), token, 0, 36, 0, 32)) {
                    revert(0, 0)
                }
                bal := mload(0)
            }
            // G--G-- callApprove G--G--
            // Calls ERC20 approve. Handles tokens that return nothing (e.g. USDT)
            // or return bool. Returns true only if the call succeeded AND
            // the return data (if any) is true.
            function callApprove(token, spender, amount) -> success {
                mstore(0, shl(224, 0x095ea7b3))
                mstore(4, spender)
                mstore(36, amount)
                success := call(gas(), token, 0, 0, 68, 0, 32)
                if success {
                    if returndatasize() {
                        returndatacopy(0, 0, 32)
                        if iszero(mload(0)) {
                            success := 0
        }
    }
}
            }
            // G--G-- callLiquidation G--G--
            // Calls Aave V3 Pool.liquidationCall.
            function callLiquidation(pool, collateralAsset, debtAsset, user, debtToCover, receiveAToken) -> success {
                mstore(0, shl(224, 0x00a718a9))
                mstore(4, collateralAsset)
                mstore(36, debtAsset)
                mstore(68, user)
                mstore(100, debtToCover)
                mstore(132, receiveAToken)
                success := call(gas(), pool, 0, 0, 164, 0, 0)
            }
            // G--G-- callSwapExactTokens G--G--
            // Calls Uniswap V2 compatible swapExactTokensForTokens with a 2-hop path.
            // Builds the ABI-encoded calldata in scratch memory and executes the call.
            function callSwapExactTokens(router, amountIn, amountOutMin, tokenIn, tokenOut, to, deadline) -> success {
                // Memory layout:
                //   [0x00:0x04)  selector
                //   [0x04:0x24)  amountIn
                //   [0x24:0x44)  amountOutMin
                //   [0x44:0x64)  path offset (160 = 0xA0)
                //   [0x64:0x84)  to
                //   [0x84:0xA4)  deadline
                //   [0xA4:0xC4)  path.length (= 2)
                //   [0xC4:0xE4)  path[0] (tokenIn)
                //   [0xE4:0x104) path[1] (tokenOut)
                // path offset = 0xA0 = 160 (bytes from 0x04 to 0xA4)
                // G-- Write fixed parameters G--
                mstore(0, shl(224, 0x38ed1739)) // SEL_SWAP_EXACT_TOKENS
                mstore(4, amountIn)
                mstore(36, amountOutMin)
                mstore(68, 160)
                mstore(100, to)
                mstore(132, deadline)
                // G-- Write dynamic path array G--
                mstore(164, 2)           // path.length
                mstore(196, tokenIn)
                mstore(228, tokenOut)
                success := call(gas(), router, 0, 0, 260, 0, 0)
            }
            // G--G-- revertWithError G--G--
            // Reverts with a 4-byte custom error selector.
            function revertWithError(selector) {
                mstore(0, shl(224, selector))
                revert(0, 4)
            }
        }
    }
}
