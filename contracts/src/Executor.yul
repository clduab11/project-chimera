object "Executor" {
    // The deployment tooling appends one ABI word containing the construction owner.
    code {
        let runtimeSize := datasize("runtime")
        let baseSize := add(dataoffset("runtime"), runtimeSize)

        // Reject bare or malformed creation bytecode. This contract has no lazy owner init.
        if iszero(eq(codesize(), add(baseSize, 32))) { revert(0, 0) }
        codecopy(0, baseSize, 32)
        let constructionOwner := mload(0)
        if or(iszero(constructionOwner), shr(160, constructionOwner)) { revert(0, 0) }
        sstore(0, constructionOwner)

        datacopy(0, dataoffset("runtime"), runtimeSize)
        return(0, runtimeSize)
    }

    object "runtime" {
        code {
            // Storage:
            //   slot 0: owner
            //   slot 1: configured Aave V3 Pool
            //   slot 2: root slot for mapping(address => bool) authorizedWorkers

            let sig := shr(224, calldataload(0))
            switch sig

            // execute(bytes)
            // Request payload is exactly 11 ABI words (352 bytes):
            // asset, amount, collateralAsset, userToLiquidate, debtToCover,
            // receiveAToken, dexRouter, amountOutMin, minProfit, tip, deadline.
            case 0x09c5eabe {
                if callvalue() { revertWithError(0xc4cae92f) }

                let sender := caller()
                if iszero(or(eq(sender, sload(0)), sload(workerSlot(sender)))) {
                    revertWithError(0x82b42900) // Unauthorized()
                }

                let poolAddress := sload(1)
                if or(iszero(poolAddress), iszero(extcodesize(poolAddress))) {
                    revertWithError(0x2083cd40) // InvalidPool()
                }

                // Canonical ABI for execute(bytes): selector, offset, length, data.
                if iszero(eq(calldatasize(), 420)) { revertWithError(0xc4cae92f) }
                if iszero(eq(calldataload(4), 32)) { revertWithError(0xc4cae92f) }
                if iszero(eq(calldataload(36), 352)) { revertWithError(0xc4cae92f) }

                let dataStart := 68
                let asset := calldataload(dataStart)
                let amount := calldataload(add(dataStart, 32))
                let collateralAsset := calldataload(add(dataStart, 64))
                let userToLiquidate := calldataload(add(dataStart, 96))
                let debtToCover := calldataload(add(dataStart, 128))
                let receiveAToken := calldataload(add(dataStart, 160))
                let dexRouter := calldataload(add(dataStart, 192))
                let amountOutMin := calldataload(add(dataStart, 224))
                let deadline := calldataload(add(dataStart, 320))

                if or(shr(160, asset), shr(160, collateralAsset)) { revertWithError(0xc4cae92f) }
                if or(shr(160, userToLiquidate), shr(160, dexRouter)) { revertWithError(0xc4cae92f) }
                if or(iszero(asset), iszero(amount)) { revertWithError(0xc4cae92f) }
                if or(iszero(collateralAsset), iszero(userToLiquidate)) { revertWithError(0xc4cae92f) }
                if iszero(debtToCover) { revertWithError(0xc4cae92f) }
                if gt(receiveAToken, 1) { revertWithError(0xc4cae92f) }
                if or(iszero(extcodesize(asset)), iszero(extcodesize(collateralAsset))) {
                    revertWithError(0xc4cae92f)
                }
                if iszero(eq(collateralAsset, asset)) {
                    if or(iszero(dexRouter), iszero(amountOutMin)) { revertWithError(0xd7c4b506) }
                    if iszero(deadline) { revertWithError(0xd7c4b506) }
                    if iszero(extcodesize(dexRouter)) { revertWithError(0xd7c4b506) }
                }

                // flashLoanSimple(address receiver,address asset,uint256 amount,
                //                 bytes params,uint16 referralCode)
                // The callback params are the final nine request words (288 bytes).
                mstore(0, shl(224, 0x42b0b77c))
                mstore(4, address())
                mstore(36, asset)
                mstore(68, amount)
                mstore(100, 160) // offset to bytes length, relative to byte 4
                mstore(132, 0)   // referralCode
                mstore(164, 288)
                calldatacopy(196, add(dataStart, 64), 288)

                if iszero(call(gas(), poolAddress, 0, 0, 484, 0, 0)) {
                    let returnSize := returndatasize()
                    if returnSize {
                        returndatacopy(0, 0, returnSize)
                        revert(0, returnSize)
                    }
                    revertWithError(0xc4cae92f) // AtomicFail()
                }
                stop()
            }

            // Aave V3 IFlashLoanSimpleReceiver.executeOperation(...)
            case 0x1b11d0ff {
                if callvalue() { revertWithError(0xc4cae92f) }

                // Canonical callback ABI with exactly nine strategy words.
                if iszero(eq(calldatasize(), 484)) { revertWithError(0xc4cae92f) }
                if iszero(eq(calldataload(132), 160)) { revertWithError(0xc4cae92f) }
                if iszero(eq(calldataload(164), 288)) { revertWithError(0xc4cae92f) }

                let asset := calldataload(4)
                let amount := calldataload(36)
                let premium := calldataload(68)
                let initiator := calldataload(100)

                let poolAddress := sload(1)
                if iszero(eq(caller(), poolAddress)) {
                    revertWithError(0x2083cd40) // InvalidPool()
                }
                if iszero(eq(initiator, address())) {
                    revertWithError(0x82b42900) // Unauthorized()
                }

                let paramsDataStart := 196
                let collateralAsset := calldataload(paramsDataStart)
                let userToLiquidate := calldataload(add(paramsDataStart, 32))
                let debtToCover := calldataload(add(paramsDataStart, 64))
                let receiveAToken := calldataload(add(paramsDataStart, 96))
                let dexRouter := calldataload(add(paramsDataStart, 128))
                let amountOutMin := calldataload(add(paramsDataStart, 160))
                let minProfit := calldataload(add(paramsDataStart, 192))
                let tip := calldataload(add(paramsDataStart, 224))
                let deadline := calldataload(add(paramsDataStart, 256))

                if or(shr(160, asset), shr(160, initiator)) { revertWithError(0xc4cae92f) }
                if or(shr(160, collateralAsset), shr(160, userToLiquidate)) {
                    revertWithError(0xc4cae92f)
                }
                if shr(160, dexRouter) { revertWithError(0xc4cae92f) }
                if or(iszero(asset), iszero(amount)) { revertWithError(0xc4cae92f) }
                if or(iszero(collateralAsset), iszero(userToLiquidate)) { revertWithError(0xc4cae92f) }
                if iszero(debtToCover) { revertWithError(0xc4cae92f) }
                if gt(receiveAToken, 1) { revertWithError(0xc4cae92f) }
                if iszero(eq(collateralAsset, asset)) {
                    if or(iszero(dexRouter), iszero(amountOutMin)) {
                        revertWithError(0xd7c4b506) // InvalidDexRouter()
                    }
                    if iszero(deadline) {
                        revertWithError(0xd7c4b506) // InvalidDexRouter()
                    }
                }

                let self := address()
                let balanceBefore := callBalanceOf(asset, self)

                if iszero(callApprove(asset, poolAddress, debtToCover)) {
                    revertWithError(0xc4cae92f)
                }
                if iszero(callLiquidation(
                    poolAddress,
                    collateralAsset,
                    asset,
                    userToLiquidate,
                    debtToCover,
                    receiveAToken
                )) {
                    revertWithError(0xc4cae92f)
                }

                if iszero(eq(collateralAsset, asset)) {
                    let collateralBalance := callBalanceOf(collateralAsset, self)
                    if iszero(callApprove(collateralAsset, dexRouter, collateralBalance)) {
                        revertWithError(0xc4cae92f)
                    }
                    if iszero(callSwapExactTokens(
                        dexRouter,
                        collateralBalance,
                        amountOutMin,
                        collateralAsset,
                        asset,
                        self,
                        deadline
                    )) {
                        revertWithError(0xc4cae92f)
                    }
                }

                let repayAmount := add(amount, premium)
                if lt(repayAmount, amount) { revertWithError(0xc4cae92f) }
                if iszero(callApprove(asset, poolAddress, repayAmount)) {
                    revertWithError(0xc4cae92f)
                }

                // The flash amount is already included in balanceBefore. Require
                // enough incremental balance for premium, configured profit, and tip.
                let balanceAfter := callBalanceOf(asset, self)
                let requiredBalance := add(balanceBefore, premium)
                if lt(requiredBalance, balanceBefore) { revertWithError(0x9b89663c) }
                let nextRequired := add(requiredBalance, minProfit)
                if lt(nextRequired, requiredBalance) { revertWithError(0x9b89663c) }
                requiredBalance := add(nextRequired, tip)
                if lt(requiredBalance, nextRequired) { revertWithError(0x9b89663c) }
                if iszero(gt(balanceAfter, requiredBalance)) {
                    revertWithError(0x9b89663c) // ProfitGateFailed()
                }

                let profit := sub(sub(balanceAfter, balanceBefore), premium)
                mstore(0, profit)
                log1(0, 32, 0x357d905f1831209797df4d55d79c5c5bf1d9f7311c976afd05e13d881eab9bc8)

                mstore(0, 1)
                return(0, 32)
            }

            // owner()
            case 0x8da5cb5b {
                if iszero(eq(calldatasize(), 4)) { revertWithError(0xc4cae92f) }
                mstore(0, sload(0))
                return(0, 32)
            }

            // pool()
            case 0x16f0115b {
                if iszero(eq(calldatasize(), 4)) { revertWithError(0xc4cae92f) }
                mstore(0, sload(1))
                return(0, 32)
            }

            // setPool(address). Setting zero deliberately disables execution.
            case 0x4437152a {
                requireOwner()
                if iszero(eq(calldatasize(), 36)) { revertWithError(0xc4cae92f) }
                let newPool := calldataload(4)
                if shr(160, newPool) { revertWithError(0xc4cae92f) }
                sstore(1, newPool)
                stop()
            }

            // setWorker(address,bool)
            case 0xc373d7f3 {
                requireOwner()
                if iszero(eq(calldatasize(), 68)) { revertWithError(0xc4cae92f) }
                let worker := calldataload(4)
                let enabled := calldataload(36)
                if or(iszero(worker), shr(160, worker)) { revertWithError(0xc4cae92f) }
                if gt(enabled, 1) { revertWithError(0xc4cae92f) }
                sstore(workerSlot(worker), enabled)
                stop()
            }

            // isWorker(address)
            case 0xaa156645 {
                if iszero(eq(calldatasize(), 36)) { revertWithError(0xc4cae92f) }
                let worker := calldataload(4)
                if shr(160, worker) { revertWithError(0xc4cae92f) }
                mstore(0, iszero(iszero(sload(workerSlot(worker)))))
                return(0, 32)
            }

            // withdraw(address,uint256)
            case 0xf3fef3a3 {
                requireOwner()
                if iszero(eq(calldatasize(), 68)) { revertWithError(0xc4cae92f) }
                let token := calldataload(4)
                let amount := calldataload(36)
                if shr(160, token) { revertWithError(0xc4cae92f) }

                if iszero(token) {
                    let available := selfbalance()
                    let withdrawal := amount
                    if iszero(withdrawal) { withdrawal := available }
                    if gt(withdrawal, available) { revertWithError(0x750b219c) }
                    if iszero(call(gas(), caller(), withdrawal, 0, 0, 0, 0)) {
                        revertWithError(0x750b219c) // WithdrawFailed()
                    }
                    stop()
                }

                let available := callBalanceOf(token, address())
                let withdrawal := amount
                if iszero(withdrawal) { withdrawal := available }
                if gt(withdrawal, available) { revertWithError(0x750b219c) }
                if iszero(callTransfer(token, caller(), withdrawal)) {
                    revertWithError(0x750b219c)
                }
                stop()
            }

            // transferOwnership(address)
            case 0xf2fde38b {
                requireOwner()
                if iszero(eq(calldatasize(), 36)) { revertWithError(0xc4cae92f) }
                let newOwner := calldataload(4)
                if or(iszero(newOwner), shr(160, newOwner)) {
                    revertWithError(0x82b42900)
                }
                sstore(0, newOwner)
                stop()
            }

            default {
                revertWithError(0x82b42900)
            }

            function workerSlot(worker) -> slot {
                mstore(0, worker)
                mstore(32, 2)
                slot := keccak256(0, 64)
            }

            function requireOwner() {
                if iszero(eq(caller(), sload(0))) {
                    revertWithError(0x82b42900)
                }
            }

            function callBalanceOf(token, account) -> tokenBal {
                mstore(0, shl(224, 0x70a08231))
                mstore(4, account)
                if iszero(staticcall(gas(), token, 0, 36, 0, 32)) { revert(0, 0) }
                if lt(returndatasize(), 32) { revert(0, 0) }
                tokenBal := mload(0)
            }

            function callApprove(token, spender, amount) -> success {
                mstore(0, shl(224, 0x095ea7b3))
                mstore(4, spender)
                mstore(36, amount)
                success := call(gas(), token, 0, 0, 68, 0, 32)
                if success {
                    let returnSize := returndatasize()
                    if returnSize {
                        if lt(returnSize, 32) {
                            success := 0
                        }
                        if iszero(lt(returnSize, 32)) {
                            success := iszero(iszero(mload(0)))
                        }
                    }
                }
            }

            function callTransfer(token, recipient, amount) -> success {
                mstore(0, shl(224, 0xa9059cbb))
                mstore(4, recipient)
                mstore(36, amount)
                success := call(gas(), token, 0, 0, 68, 0, 32)
                if success {
                    let returnSize := returndatasize()
                    if returnSize {
                        if lt(returnSize, 32) {
                            success := 0
                        }
                        if iszero(lt(returnSize, 32)) {
                            success := iszero(iszero(mload(0)))
                        }
                    }
                }
            }

            function callLiquidation(poolAddress, collateralAsset, debtAsset, user, debtToCover, receiveAToken) -> success {
                mstore(0, shl(224, 0x00a718a9))
                mstore(4, collateralAsset)
                mstore(36, debtAsset)
                mstore(68, user)
                mstore(100, debtToCover)
                mstore(132, receiveAToken)
                success := call(gas(), poolAddress, 0, 0, 164, 0, 0)
            }

            function callSwapExactTokens(router, amountIn, amountOutMin, tokenIn, tokenOut, recipient, deadline) -> success {
                mstore(0, shl(224, 0x38ed1739))
                mstore(4, amountIn)
                mstore(36, amountOutMin)
                mstore(68, 160)
                mstore(100, recipient)
                mstore(132, deadline)
                mstore(164, 2)
                mstore(196, tokenIn)
                mstore(228, tokenOut)
                success := call(gas(), router, 0, 0, 260, 0, 0)
            }

            function revertWithError(selector) {
                mstore(0, shl(224, selector))
                revert(0, 4)
            }
        }
    }
}
