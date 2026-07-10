// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";

interface IERC20Balance {
    function balanceOf(address account) external view returns (uint256);
}

/// @notice Opt-in Base mainnet fork configuration smoke test for the standalone Executor.
/// @dev Set BASE_FORK_URL to enable. This test never broadcasts a live transaction.
contract ExecutorBaseForkTest is Test {
    address internal constant BASE_AAVE_V3_POOL =
        0xA238Dd80C259a72e81d7e4664a9801593F98d1c5;

    bytes4 internal constant SEL_POOL = 0x16f0115b;
    bytes4 internal constant SEL_SET_POOL = 0x4437152a;
    bytes4 internal constant SEL_SET_WORKER = 0xc373d7f3;
    bytes4 internal constant SEL_IS_WORKER = 0xaa156645;
    bytes4 internal constant SEL_EXECUTE = 0x09c5eabe;

    function testBaseForkStandaloneExecutorConfiguration() public {
        string memory forkUrl = vm.envOr("BASE_FORK_URL", string(""));
        if (bytes(forkUrl).length == 0) return;

        vm.createSelectFork(forkUrl);
        assertEq(block.chainid, 8453, "BASE_FORK_URL is not Base mainnet");
        assertGt(BASE_AAVE_V3_POOL.code.length, 0, "canonical Pool has no code");

        address executor = _deployExecutor(address(this));
        address worker = makeAddr("baseForkWorker");

        (bool poolSet, ) = executor.call(
            abi.encodeWithSelector(SEL_SET_POOL, BASE_AAVE_V3_POOL)
        );
        assertTrue(poolSet, "setPool failed");

        (bool workerSet, ) = executor.call(
            abi.encodeWithSelector(SEL_SET_WORKER, worker, true)
        );
        assertTrue(workerSet, "setWorker failed");

        (bool poolOk, bytes memory poolData) = executor.staticcall(
            abi.encodeWithSelector(SEL_POOL)
        );
        assertTrue(poolOk, "pool view failed");
        assertEq(
            abi.decode(poolData, (address)),
            BASE_AAVE_V3_POOL,
            "wrong configured Pool"
        );

        (bool workerOk, bytes memory workerData) = executor.staticcall(
            abi.encodeWithSelector(SEL_IS_WORKER, worker)
        );
        assertTrue(workerOk, "isWorker failed");
        assertTrue(abi.decode(workerData, (bool)), "worker not authorized");
    }

    /// @notice Executes a real liquidation only when every opportunity input is explicit.
    /// @dev Required env: BASE_FORK_URL, BASE_LIQUIDATION_USER,
    ///      BASE_COLLATERAL_TOKEN, BASE_DEBT_TOKEN, BASE_DEBT_AMOUNT,
    ///      BASE_DEX_ROUTER, and BASE_AMOUNT_OUT_MIN. BASE_MIN_PROFIT is optional.
    ///      BASE_DEBT_AMOUNT is both the flash-loan amount and debtToCover.
    ///      Positions and quotes go stale; a revert means the supplied opportunity is
    ///      no longer liquidatable under these parameters. This test never broadcasts.
    function testBaseForkRealLiquidationWhenConfigured() public {
        string memory forkUrl = vm.envOr("BASE_FORK_URL", string(""));
        if (bytes(forkUrl).length == 0) return;

        address user = vm.envOr("BASE_LIQUIDATION_USER", address(0));
        address collateralToken = vm.envOr("BASE_COLLATERAL_TOKEN", address(0));
        address debtToken = vm.envOr("BASE_DEBT_TOKEN", address(0));
        uint256 debtAmount = vm.envOr("BASE_DEBT_AMOUNT", uint256(0));
        address dexRouter = vm.envOr("BASE_DEX_ROUTER", address(0));
        uint256 amountOutMin = vm.envOr("BASE_AMOUNT_OUT_MIN", uint256(0));

        if (
            user == address(0) || collateralToken == address(0)
                || debtToken == address(0) || debtAmount == 0
                || dexRouter == address(0) || amountOutMin == 0
        ) return;

        uint256 minProfit = vm.envOr("BASE_MIN_PROFIT", uint256(0));

        vm.createSelectFork(forkUrl);
        assertEq(block.chainid, 8453, "BASE_FORK_URL is not Base mainnet");
        assertGt(BASE_AAVE_V3_POOL.code.length, 0, "canonical Pool has no code");
        assertGt(collateralToken.code.length, 0, "collateral token has no code");
        assertGt(debtToken.code.length, 0, "debt token has no code");
        assertGt(dexRouter.code.length, 0, "DEX router has no code");

        address executor = _deployExecutor(address(this));
        address worker = makeAddr("baseLiquidationWorker");

        (bool poolSet, ) = executor.call(
            abi.encodeWithSelector(SEL_SET_POOL, BASE_AAVE_V3_POOL)
        );
        require(poolSet, "setPool failed");

        (bool workerSet, ) = executor.call(
            abi.encodeWithSelector(SEL_SET_WORKER, worker, true)
        );
        require(workerSet, "setWorker failed");

        bytes memory request = abi.encode(
            debtToken,
            debtAmount,
            collateralToken,
            user,
            debtAmount,
            false,
            dexRouter,
            amountOutMin,
            minProfit,
            uint256(0),
            block.timestamp + 5 minutes
        );
        assertEq(request.length, 11 * 32, "request must be exactly eleven words");
        assertEq(
            IERC20Balance(debtToken).balanceOf(executor),
            0,
            "new Executor has a debt-token balance"
        );

        vm.prank(worker);
        (bool success, ) = executor.call(
            abi.encodeWithSelector(SEL_EXECUTE, request)
        );
        require(
            success,
            "liquidation reverted: env-driven position or quote is no longer liquidatable"
        );
        assertGt(
            IERC20Balance(debtToken).balanceOf(executor),
            0,
            "real liquidation retained no debt-token profit"
        );
    }

    function _deployExecutor(address initialOwner) internal returns (address deployed) {
        string memory path = string.concat(
            vm.projectRoot(),
            "/out/Executor.yul/Executor.json"
        );
        bytes memory creationCode = vm.parseJsonBytes(
            vm.readFile(path),
            ".bytecode.object"
        );
        bytes memory initCode = abi.encodePacked(creationCode, abi.encode(initialOwner));
        assembly {
            deployed := create(0, add(initCode, 32), mload(initCode))
        }
        require(deployed != address(0), "Executor deployment failed");
    }
}
