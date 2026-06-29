/*
 * Project Chimera - Flash Loan Executor (Complete Yul Implementation)
 * ==================================================================
 * Aave V3 flash-loan receiver + liquidation engine + DEX swap router.
 * All-or-nothing atomic execution with profit gate. EIP-7702 compatible.
 * Gas optimized for L2 (Cancun). Production ready.
 *
 * Architecture:
 *   1. Aave Pool calls executeOperation() during flashLoanSimple().
 *   2. Executor decodes strategy params from the bytes payload.
 *   3. Atomic steps: (a) liquidationCall, (b) swap collateral -> debt,
 *      (c) approve Pool for repayment, (d) profit gate check.
 *   4. If any step fails, the entire transaction reverts (all-or-nothing).
 *   5. On success, emits Profit event and returns true to Aave.
 *
 * EIP-7702:
 *   When this runtime is attached to an EOA via EIP-7702, address()
 *   returns the EOA address and caller() is the Aave Pool.
 *   Lazy-init: first caller becomes owner (slot 0). Owner manages
 *   pool address (slot 1) via setPool() and can withdraw via withdraw().
 *
 * DEX Routing:
 *   Supports multiple DEX routers by passing the router address in
 *   strategy params. Uses selector-based routing for swap functions.
 *   Currently implements Uniswap-V2-compatible swapExactTokensForTokens.
 *   Extending to V3 or other routers only requires adding a new
 *   selector branch and encoding helper.
 *
 * Storage Layout:
 *   slot 0: owner address (set lazily on first call)
 *   slot 1: aave pool address (set by owner via setPool)
 */
object "Executor" {
    // ================================= Constructor =================================
    // Optionally sets the owner from an appended 32-byte address arg, then
    // copies runtime bytecode to memory and returns it.
    code {
        // Constructor: optionally set owner from an appended 32-byte address arg.
        // Standard CREATE appends ABI-encoded constructor args after the init code.
        // If a non-zero address is present, slot 0 (owner) is set at construction.
        // If absent or zero, slot 0 stays 0 and runtime lazy-init applies.
        let argOffset := add(dataoffset("runtime"), datasize("runtime"))
        if gt(codesize(), argOffset) {
            codecopy(0, argOffset, 32)
            let ownerArg := and(mload(0), 0xffffffffffffffffffffffffffffffffffffffff)
            if iszero(iszero(ownerArg)) {
                sstore(0, ownerArg)
            }
        }
        datacopy(0, dataoffset("runtime"), datasize("runtime"))
        return(0, datasize("runtime"))
    }
    object "runtime" {
        code {
            // ==================== SECTION 0: CONSTANTS ====================

            // -- Function selectors --
            let SEL_EXECUTE_OPERATION := 0x1b11d0ff // executeOperation(address,uint256,uint256,address,bytes)
            let SEL_EXEC              := 0x55f86501 // exec(bytes)
            let SEL_OWNER             := 0x8da5cb5b // owner()
            let SEL_SET_POOL          := 0xa51b62c1 // setPool(address)
            let SEL_TRANSFER_OWNERSHIP := 0xf2fde38b // transferOwnership(address)
            let SEL_WITHDRAW          := 0xf3fef3a3 // withdraw(address,uint256)

            // -- Aave / DEX selectors --
            let SEL_LIQUIDATION_CALL  := 0x00a718a9 // liquidationCall(address,address,address,uint256,bool)
            let SEL_SWAP_EXACT_TOKENS := 0x38ed1739 // swapExactTokensForTokens(uint256,uint256,address[],address,uint256)

            // -- ERC20 selectors --
            let SEL_BALANCE_OF := 0x70a08231 // balanceOf(address)
            let SEL_APPROVE    := 0x095ea7b3 // approve(address,uint256)
            let SEL_TRANSFER   := 0xa9059cbb // transfer(address,uint256)

            // -- Event --
            let EVT_PROFIT_TOPIC0 := 0x357d905f1831209797df4d55d79c5c5bf1d9f7311c976afd05e13d881eab9bc8

            // -- Custom error selectors --
            let ERR_PROFIT_GATE      := 0x2e5a0d02 // ProfitGateFailed()
            let ERR_ATOMIC_FAIL      := 0x5fe2e75c // AtomicFail()
            let ERR_UNAUTHORIZED     := 0x82b42900 // Unauthorized()
            let ERR_INVALID_ROUTER   := 0x8d4f59a9 // InvalidDexRouter()
            let ERR_INVALID_POOL     := 0xd0363b78 // InvalidPool()
            let ERR_WITHDRAW_FAILED  := 0xf1620b3e // WithdrawFailed()

            // ==================== SECTION 0.5: LAZY OWNER INIT ====================
            // EIP-7702 compat: if slot 0 is zero, store caller() as owner.
            if iszero(sload(0)) {
                sstore(0, caller())
            }

            // ==================== SECTION 1: DISPATCHER ====================
            let sig := shr(224, calldataload(0))
            switch sig

            // -------------------- CASE: owner() --------------------
            case 0x8da5cb5b {
                mstore(0, sload(0))
                return(0, 32)
            }

            // -------------------- CASE: setPool(address) --------------------
            case 0xa51b62c1 {
                if iszero(eq(caller(), sload(0))) {
                    mstore(0, shl(224, ERR_UNAUTHORIZED))
                    revert(0, 4)
                }
                let poolAddr := shr(96, shl(96, calldataload(4)))
                sstore(1, poolAddr)
                mstore(0, 1)
                return(0, 32)
            }

            // -------------------- CASE: transferOwnership(address) --------------------
            case 0xf2fde38b {
                // owner-gated
                if iszero(eq(caller(), sload(0))) {
                    mstore(0, shl(224, ERR_UNAUTHORIZED))
                    revert(0, 4)
                }
                let newOwner := and(calldataload(4), 0xffffffffffffffffffffffffffffffffffffffff)
                // Reject zero address (would re-enable lazy-init hijack on next call)
                if iszero(newOwner) {
                    mstore(0, shl(224, ERR_UNAUTHORIZED))
                    revert(0, 4)
                }
                sstore(0, newOwner)
                mstore(0, 1)
                return(0, 32)
            }

            // -------------------- CASE: withdraw(address,uint256) --------------------
            case 0xf3fef3a3 {
                if iszero(eq(caller(), sload(0))) {
                    mstore(0, shl(224, ERR_UNAUTHORIZED))
                    revert(0, 4)
                }
                let w_token  := shr(96, shl(96, calldataload(4)))
                let w_amount := calldataload(36)
                let w_owner  := sload(0)

                if iszero(w_token) {
                    // Withdraw native ETH
                    if iszero(w_amount) { w_amount := selfbalance() }
                    let w_ok := call(gas(), w_owner, w_amount, 0, 0, 0, 0)
                    if iszero(w_ok) {
                        mstore(0, shl(224, ERR_WITHDRAW_FAILED))
                        revert(0, 4)
                    }
                } {
                    // Withdraw ERC20
                    if iszero(w_amount) {
                        mstore(0, shl(224, SEL_BALANCE_OF))
                        mstore(4, address())
                        if iszero(staticcall(gas(), w_token, 0, 36, 0, 32)) {
                            mstore(0, shl(224, ERR_WITHDRAW_FAILED))
                            revert(0, 4)
                        }
                        w_amount := mload(0)
                    }
                    mstore(0, shl(224, SEL_TRANSFER))
                    mstore(4, w_owner)
                    mstore(36, w_amount)
                    let w_ok := call(gas(), w_token, 0, 0, 68, 0, 32)
                    if w_ok {
                        if returndatasize() {
                            returndatacopy(0, 0, 32)
                            if iszero(mload(0)) { w_ok := 0 }
                        }
                    }
                    if iszero(w_ok) {
                        mstore(0, shl(224, ERR_WITHDRAW_FAILED))
                        revert(0, 4)
                    }
                }
                stop()
            }

            // ======================================================================
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

                // -- Pool validation: caller must be the configured pool --
                if iszero(eq(caller(), sload(1))) {
                    mstore(0, shl(224, ERR_INVALID_POOL))
                    revert(0, 4)
                }

                // -- Decode fixed arguments --
                let asset     := calldataload(4)
                let amount    := calldataload(36)
                let premium   := calldataload(68)
                let initiator := calldataload(100)

                // -- Decode dynamic bytes (params) --
                let paramsOffset    := add(calldataload(132), 4)
                let paramsLen       := calldataload(paramsOffset)
                let paramsDataStart := add(paramsOffset, 32)

                // -- Validate params length --
                // StrategyParams: 9 * 32 = 288 bytes.
                if lt(paramsLen, 288) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }

                // -- Decode StrategyParams --
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

                // -- Record pre-flight balance of debt token --
                let self := address()
                let balanceBefore := callBalanceOf(asset, self)

                // -- Step 1: LIQUIDATION --
                // Approve Aave Pool to pull debtToCover of the debt asset.
                let pool := caller()
                if iszero(callApprove(asset, pool, debtToCover)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                // Call liquidationCall on Aave Pool.
                if iszero(callLiquidation(pool, collateralAsset, asset, userToLiquidate, debtToCover, receiveAToken)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }

                // -- Step 2: DEX SWAP --
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

                // -- Step 3: REPAY FLASH LOAN --
                // Approve Aave Pool to pull back flash-loaned amount + premium.
                let repayAmt := add(amount, premium)
                if iszero(callApprove(asset, pool, repayAmt)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }

                // -- Step 4: PROFIT GATE (with overflow protection) --
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

                // -- Step 5: EMIT PROFIT EVENT --
                let profit := sub(balanceAfter, balanceBefore)
                mstore(0, profit)
                log1(0, 32, EVT_PROFIT_TOPIC0)

                // -- Step 6: RETURN TRUE TO AAVE --
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

                // -- Liquidation --
                if iszero(callApprove(d_asset, d_pool, d_debtToCover)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }
                if iszero(callLiquidation(d_pool, d_collateral, d_asset, d_user, d_debtToCover, d_receiveAToken)) {
                    revertWithError(ERR_ATOMIC_FAIL)
                }

                // -- Swap --
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

                // -- Profit Gate (with overflow protection) --
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

                // -- Emit Profit Event --
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

            // ==================== SECTION 2: INTERNAL HELPERS ====================

            // -- callBalanceOf --
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

            // -- callApprove --
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

            // -- callLiquidation --
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

            // -- callSwapExactTokens --
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
                mstore(0, shl(224, 0x38ed1739))
                mstore(4, amountIn)
                mstore(36, amountOutMin)
                mstore(68, 160)
                mstore(100, to)
                mstore(132, deadline)
                mstore(164, 2)
                mstore(196, tokenIn)
                mstore(228, tokenOut)
                success := call(gas(), router, 0, 0, 260, 0, 0)
            }

            // -- revertWithError --
            // Reverts with a 4-byte custom error selector.
            function revertWithError(selector) {
                mstore(0, shl(224, selector))
                revert(0, 4)
            }
        }
    }
}
