// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {IAavePool} from "../src/interfaces/IAavePool.sol";
import {IDexRouter} from "../src/interfaces/IDexRouter.sol";

contract MockERC20 {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    function mint(address to, uint256 amount) public { balanceOf[to] += amount; }
    function approve(address spender, uint256) public returns (bool) { return true; }
    function transfer(address to, uint256 amount) public returns (bool) {
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

contract MockPool {
    address public expectedVictim;
    MockERC20 public collateralToken;
    MockERC20 public debtToken;

    function setExpectations(address victim, MockERC20 _collateral, MockERC20 _debt) public {
        expectedVictim = victim;
        collateralToken = _collateral;
        debtToken = _debt;
    }

    function liquidationCall(address collateralAsset, address debtAsset, address user, uint256 debtToCover, bool receiveAToken) external {
        require(user == expectedVictim, "wrong victim");
        collateralToken.mint(msg.sender, 700 ether);
    }
}

contract MockRouter {
    MockERC20 public tokenIn;
    MockERC20 public tokenOut;
    uint256 public amountToMint = 1200 ether;

    function setTokens(MockERC20 _tokenIn, MockERC20 _tokenOut) public {
        tokenIn = _tokenIn;
        tokenOut = _tokenOut;
    }

    function setAmountToMint(uint256 amount) public {
        amountToMint = amount;
    }

    function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external returns (uint[] memory amounts) {
        tokenOut.mint(to, amountToMint);
    }
}

contract ExecutorTest is Test {
    address executor;
    MockPool mockPool;
    MockERC20 mockTokenA;
    MockERC20 mockTokenB;
    MockRouter mockRouter;
    address victim = makeAddr("victim");

    // Allow receiving ETH from Executor.withdraw(0,0)
    receive() external payable {}

    bytes4 constant ERR_PROFIT_GATE      = 0x2e5a0d02;
    bytes4 constant ERR_ATOMIC_FAIL      = 0x5fe2e75c;
    bytes4 constant ERR_UNAUTHORIZED     = 0x82b42900;
    bytes4 constant ERR_INVALID_ROUTER   = 0x8d4f59a9;
    bytes4 constant ERR_INVALID_POOL     = 0xd0363b78;
    bytes4 constant SEL_EXECUTE_OPERATION = 0x1b11d0ff;
    bytes4 constant SEL_EXEC             = 0x55f86501;
    bytes4 constant SEL_OWNER            = 0x8da5cb5b;
    bytes4 constant SEL_SET_POOL         = 0xa51b62c1;
    bytes4 constant SEL_WITHDRAW         = 0xf3fef3a3;
    bytes4 constant SEL_TRANSFER_OWNERSHIP = 0xf2fde38b;
    bytes32 constant EVT_PROFIT_TOPIC0   = 0x357d905f1831209797df4d55d79c5c5bf1d9f7311c976afd05e13d881eab9bc8;

    uint256 constant FLASH_AMOUNT   = 1000 ether;
    uint256 constant FLASH_PREMIUM  = 5 ether;
    uint256 constant DEBT_TO_COVER  = 800 ether;
    uint256 constant COLLATERAL_BAL = 700 ether;
    uint256 constant SWAP_OUT       = 1200 ether;
    uint256 constant AMOUNT_OUT_MIN = 500 ether;
    uint256 constant MIN_PROFIT     = 100 ether;
    uint256 constant TIP            = 10 ether;
    uint256 constant DEADLINE       = 2000000000;

    function setUp() public {
        mockPool      = new MockPool();
        mockTokenA    = new MockERC20();
        mockTokenB    = new MockERC20();
        mockRouter    = new MockRouter();

        mockPool.setExpectations(victim, mockTokenB, mockTokenA);
        mockRouter.setTokens(mockTokenB, mockTokenA);

        string memory path = string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json");
        string memory json = vm.readFile(path);
        bytes memory code = vm.parseJsonBytes(json, ".bytecode.object");
        // Append owner arg exactly like Deploy.s.sol so ctor reads it (no garbage read from initcode tail).
        bytes memory initCode = abi.encodePacked(code, abi.encode(address(this)));
        address _executor;
        assembly {
            _executor := create(0, add(initCode, 0x20), mload(initCode))
        }
        executor = _executor;
        require(executor != address(0), "Executor deployment failed");

        // Owner is already set via appended ctor arg; now set the pool (owner-gated).
        bytes memory setPoolData = abi.encodeWithSelector(SEL_SET_POOL, address(mockPool));
        (bool ok, ) = executor.call(setPoolData);
        require(ok, "setPool failed");
    }

    function _getParams() internal view returns (bytes memory) {
        return abi.encode(
            address(mockTokenB), victim, DEBT_TO_COVER, false, address(mockRouter),
            AMOUNT_OUT_MIN, MIN_PROFIT, TIP, DEADLINE
        );
    }

    // ─── 1. Flash Loan Callback ────────────────────────────────────────────────
    function testFlashLoanCallback() public {
        bytes memory params = _getParams();
        mockTokenA.mint(executor, 2000 ether);

        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, executor, params
        );

        vm.recordLogs();
        vm.prank(address(mockPool));
        (bool success, bytes memory ret) = executor.call(callData);
        assertTrue(success, "executeOperation should succeed");

        assertEq(ret.length, 32, "should return bool (32 bytes)");
        assertEq(abi.decode(ret, (bool)), true, "should return true");

        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool foundProfit = false;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] == EVT_PROFIT_TOPIC0) {
                foundProfit = true;
                uint256 profit = abi.decode(logs[i].data, (uint256));
                assertEq(profit, 1200 ether, "profit mismatch");
                break;
            }
        }
        assertTrue(foundProfit, "Profit event should be emitted");
    }

    // ─── 2. Profit Gate Revert ─────────────────────────────────────────────────
    function testProfitGateRevertsIfUnprofitable() public {
        mockRouter.setAmountToMint(50 ether);
        bytes memory params = _getParams();
        mockTokenA.mint(executor, 2000 ether);

        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, executor, params
        );

        vm.expectRevert(ERR_PROFIT_GATE);
        vm.prank(address(mockPool));
        (bool _ok1, ) = executor.call(callData);
    }

    // ─── 3. Atomic Revert on Liquidation Failure ───────────────────────────────
    function testAtomicRevertOnLiquidationFailure() public {
        bytes memory params = _getParams();
        mockTokenA.mint(executor, 2000 ether);

        vm.mockCallRevert(
            address(mockPool),
            abi.encodeWithSelector(mockPool.liquidationCall.selector),
            "Liquidation reverted"
        );

        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, executor, params
        );

        vm.expectRevert(ERR_ATOMIC_FAIL);
        vm.prank(address(mockPool));
        (bool _ok2, ) = executor.call(callData);
    }

    // ─── Wrong Initiator Rejected ──────────────────────────────────────────────
    function testExecuteOperationRejectsWrongInitiator() public {
        bytes memory params = _getParams();
        mockTokenA.mint(executor, 2000 ether);

        // Valid pool caller but wrong initiator (not the executor address).
        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, address(0xBEEF), params
        );

        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(address(mockPool));
        (bool _ok3, ) = executor.call(callData);
    }

    // ─── Payload Length Validation (exact sizes) ───────────────────────────────
    function testExecuteOperationRejectsWrongPayloadLength() public {
        // Too short (287 bytes instead of 288)
        bytes memory shortParams = _slice(_getParams(), 0, 287);
        mockTokenA.mint(executor, 2000 ether);
        bytes memory callDataShort = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, executor, shortParams
        );
        vm.expectRevert(ERR_ATOMIC_FAIL);
        vm.prank(address(mockPool));
        executor.call(callDataShort);

        // Too long (289 bytes)
        bytes memory longParams = abi.encodePacked(_getParams(), hex"00");
        bytes memory callDataLong = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, executor, longParams
        );
        vm.expectRevert(ERR_ATOMIC_FAIL);
        vm.prank(address(mockPool));
        executor.call(callDataLong);
    }

    function testDirectExecRejectsWrongPayloadLength() public {
        // Too short (383 bytes instead of 384)
        bytes memory shortData = _slice(abi.encode(
            address(mockTokenA), FLASH_AMOUNT, address(mockPool), address(mockTokenB),
            victim, DEBT_TO_COVER, false, address(mockRouter), AMOUNT_OUT_MIN, MIN_PROFIT, TIP, DEADLINE
        ), 0, 383);
        mockTokenA.mint(executor, 2000 ether);
        bytes memory callDataShort = abi.encodeWithSelector(SEL_EXEC, shortData);
        vm.expectRevert(ERR_ATOMIC_FAIL);
        vm.prank(address(this));
        executor.call(callDataShort);

        // Too long (385 bytes)
        bytes memory longData = abi.encodePacked(abi.encode(
            address(mockTokenA), FLASH_AMOUNT, address(mockPool), address(mockTokenB),
            victim, DEBT_TO_COVER, false, address(mockRouter), AMOUNT_OUT_MIN, MIN_PROFIT, TIP, DEADLINE
        ), hex"00");
        bytes memory callDataLong = abi.encodeWithSelector(SEL_EXEC, longData);
        vm.expectRevert(ERR_ATOMIC_FAIL);
        vm.prank(address(this));
        executor.call(callDataLong);
    }

    // ─── 4. EIP-7702 Ownership Hardening ──────────────────────────────────────
    // Arbitrary external caller must NOT be able to seize ownership of etched worker.
    function testEIP7702ArbitraryCallerCannotClaim() public {
        address worker = makeAddr("worker_eoa");
        string memory path = string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json");
        string memory json = vm.readFile(path);
        bytes memory code = vm.parseJsonBytes(json, ".deployedBytecode.object");
        vm.etch(worker, code);

        // Arbitrary external caller tries setPool — must revert Unauthorized.
        address attacker = makeAddr("attacker");
        bytes memory setPoolData = abi.encodeWithSelector(SEL_SET_POOL, address(mockPool));
        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(attacker);
        (bool ok, ) = worker.call(setPoolData);
        assertFalse(ok, "attacker must not claim ownership");
    }

    // Worker self-init path succeeds when the etched worker itself calls setPool.
    function testEIP7702WorkerSelfInitSucceeds() public {
        address worker = makeAddr("worker_eoa");
        string memory path = string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json");
        string memory json = vm.readFile(path);
        bytes memory code = vm.parseJsonBytes(json, ".deployedBytecode.object");
        vm.etch(worker, code);

        vm.prank(worker);
        bytes memory setPoolData = abi.encodeWithSelector(SEL_SET_POOL, address(mockPool));
        (bool ok, ) = worker.call(setPoolData);
        assertTrue(ok, "worker self-init must succeed");
    }

    // ─── 5. Direct Exec Path ──────────────────────────────────────────────────
    function testDirectExecPath() public {
        bytes memory strategyData = abi.encode(
            address(mockTokenA), FLASH_AMOUNT, address(mockPool), address(mockTokenB),
            victim, DEBT_TO_COVER, false, address(mockRouter), AMOUNT_OUT_MIN, MIN_PROFIT, TIP, DEADLINE
        );
        mockTokenA.mint(executor, 2000 ether);

        bytes memory callData = abi.encodeWithSelector(SEL_EXEC, strategyData);

        vm.recordLogs();
        vm.prank(address(this));
        (bool success, ) = executor.call(callData);
        assertTrue(success, "direct exec should succeed");

        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool foundProfit = false;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] == EVT_PROFIT_TOPIC0) {
                foundProfit = true;
                break;
            }
        }
        assertTrue(foundProfit, "Profit event should be emitted on direct exec");
    }

    function testExecutorStructuralIntegrity() public {
        string memory path = string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json");
        string memory json = vm.readFile(path);
        bytes memory runtimeCode = vm.parseJsonBytes(json, ".deployedBytecode.object");

        assertGt(runtimeCode.length, 0, "runtime bytecode must be non-empty");
        assertGt(runtimeCode.length, 100, "runtime bytecode suspiciously short");

        bytes4[2] memory requiredSelectors = [SEL_EXECUTE_OPERATION, SEL_EXEC];
        for (uint256 i = 0; i < requiredSelectors.length; i++) {
            bool found = false;
            bytes4 sel = requiredSelectors[i];
            for (uint256 j = 0; j + 4 <= runtimeCode.length; j++) {
                bytes4 candidate;
                assembly {
                    candidate := mload(add(add(runtimeCode, 0x20), j))
                }
                if (candidate == sel) {
                    found = true;
                    break;
                }
            }
            assertTrue(found, "runtime bytecode must contain required selector");
        }

        assertGt(executor.code.length, 0, "deployed contract must have code");
    }

    // ─── Admin surface + gates (per T2 ABI gate + Invariant #6) ─────────────────

    function testOwnerView() public {
        bytes memory data = abi.encodeWithSelector(SEL_OWNER);
        (bool ok, bytes memory ret) = executor.staticcall(data);
        assertTrue(ok, "owner() staticcall failed");
        address o = abi.decode(ret, (address));
        assertEq(o, address(this), "owner should be test contract after setUp");
    }

    function testSetPoolOnlyOwner() public {
        address stranger = makeAddr("stranger");
        bytes memory bad = abi.encodeWithSelector(SEL_SET_POOL, address(0xBEEF));
        vm.prank(stranger);
        vm.expectRevert(ERR_UNAUTHORIZED);
        (bool _ok3, ) = executor.call(bad);
    }

    function testExecuteOperationRejectsNonPool() public {
        bytes memory params = _getParams();
        mockTokenA.mint(executor, 2000 ether);
        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, executor, params
        );
        address notPool = makeAddr("not_the_pool");
        vm.prank(notPool);
        vm.expectRevert(ERR_INVALID_POOL);
        (bool _ok4, ) = executor.call(callData);
    }

    function testWithdrawERC20ByOwner() public {
        // fund executor
        mockTokenA.mint(executor, 123 ether);
        uint256 balBefore = mockTokenA.balanceOf(address(this));
        bytes memory wd = abi.encodeWithSelector(SEL_WITHDRAW, address(mockTokenA), uint256(50 ether));
        vm.prank(address(this));
        (bool ok, ) = executor.call(wd);
        assertTrue(ok, "withdraw should succeed");
        uint256 balAfter = mockTokenA.balanceOf(address(this));
        assertEq(balAfter - balBefore, 50 ether, "owner should have received 50");
    }

    function testWithdrawETHByOwner() public {
        vm.deal(executor, 1 ether);
        uint256 before = address(this).balance;
        bytes memory wd = abi.encodeWithSelector(SEL_WITHDRAW, address(0), uint256(0)); // 0 = full
        vm.prank(address(this));
        (bool ok, ) = executor.call(wd);
        assertTrue(ok, "eth withdraw should succeed");
        uint256 afterBal = address(this).balance;
        assertGt(afterBal, before, "received ETH");
    }

    function testWithdrawByNonOwnerReverts() public {
        mockTokenA.mint(executor, 10 ether);
        address stranger = makeAddr("stranger2");
        bytes memory wd = abi.encodeWithSelector(SEL_WITHDRAW, address(mockTokenA), uint256(1 ether));
        vm.prank(stranger);
        vm.expectRevert(ERR_UNAUTHORIZED);
        (bool _ok5, ) = executor.call(wd);
    }

    function testTransferOwnership() public {
        address newO = makeAddr("newOwner");
        bytes memory txo = abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, newO);
        vm.prank(address(this));
        (bool ok, ) = executor.call(txo);
        assertTrue(ok, "transferOwnership should succeed");

        // verify owner changed
        bytes memory q = abi.encodeWithSelector(SEL_OWNER);
        (bool ok2, bytes memory ret) = executor.staticcall(q);
        assertTrue(ok2);
        assertEq(abi.decode(ret, (address)), newO, "owner should be newO");
    }

    function testTransferOwnershipRejectsZero() public {
        bytes memory txo = abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, address(0));
        vm.prank(address(this));
        vm.expectRevert(ERR_UNAUTHORIZED);
        (bool _ok6, ) = executor.call(txo);
    }

    function testTransferOwnershipOnlyOwner() public {
        address stranger = makeAddr("stranger3");
        bytes memory txo = abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, makeAddr("x"));
        vm.prank(stranger);
        vm.expectRevert(ERR_UNAUTHORIZED);
        (bool _ok7, ) = executor.call(txo);
    }

    // ─── E2E: Rust-assembled StrategyParams shape matches Executor.yul calldataload offsets ──
    function testFlashLoanParamsMatchExecutorShape() public {
        bytes memory params = _getParams();
        // StrategyParams is exactly 288 bytes (9 words of 32)
        assertEq(params.length, 288, "params must be 288 bytes");

        // Decode known fields and verify offsets match Executor.yul:99-107
        // word 0 (offset 0): collateralAsset
        address decodedCollateral = address(uint160(uint256(bytes32(_slice(params, 0, 32)))));
        assertEq(decodedCollateral, address(mockTokenB), "word0: collateralAsset");

        // word 1 (offset 32): userToLiquidate
        address decodedUser = address(uint160(uint256(bytes32(_slice(params, 32, 32)))));
        assertEq(decodedUser, victim, "word1: userToLiquidate");

        // word 2 (offset 64): debtToCover
        uint256 decodedDebt = uint256(bytes32(_slice(params, 64, 32)));
        assertEq(decodedDebt, DEBT_TO_COVER, "word2: debtToCover");

        // word 3 (offset 96): receiveAToken
        uint256 decodedRat = uint256(bytes32(_slice(params, 96, 32)));
        assertEq(decodedRat, 0, "word3: receiveAToken (false=0)");

        // word 4 (offset 128): dexRouter
        address decodedRouter = address(uint160(uint256(bytes32(_slice(params, 128, 32)))));
        assertEq(decodedRouter, address(mockRouter), "word4: dexRouter");

        // word 5 (offset 160): amountOutMin
        uint256 decodedAom = uint256(bytes32(_slice(params, 160, 32)));
        assertEq(decodedAom, AMOUNT_OUT_MIN, "word5: amountOutMin");

        // word 6 (offset 192): minProfit
        uint256 decodedMinProfit = uint256(bytes32(_slice(params, 192, 32)));
        assertEq(decodedMinProfit, MIN_PROFIT, "word6: minProfit");

        // word 7 (offset 224): tip
        uint256 decodedTip = uint256(bytes32(_slice(params, 224, 32)));
        assertEq(decodedTip, TIP, "word7: tip");

        // word 8 (offset 256): deadline
        uint256 decodedDeadline = uint256(bytes32(_slice(params, 256, 32)));
        assertEq(decodedDeadline, DEADLINE, "word8: deadline");
    }

    function _slice(bytes memory data, uint256 start, uint256 len) internal pure returns (bytes memory) {
        bytes memory out = new bytes(len);
        for (uint256 i = 0; i < len; i++) {
            out[i] = data[start + i];
        }
        return out;
    }
}
