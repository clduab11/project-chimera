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

    bytes4 constant ERR_PROFIT_GATE      = 0x2e5a0d02;
    bytes4 constant ERR_ATOMIC_FAIL      = 0x5fe2e75c;
    bytes4 constant ERR_UNAUTHORIZED     = 0x82b42900;
    bytes4 constant ERR_INVALID_ROUTER   = 0x8d4f59a9;
    bytes4 constant ERR_INVALID_POOL     = 0xd0363b78;
    bytes4 constant ERR_WITHDRAW_FAILED  = 0xf1620b3e;
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
        bytes memory code  = vm.parseJsonBytes(json, ".bytecode.object");
        address _executor;
        assembly {
            _executor := create(0, add(code, 0x20), mload(code))
        }
        executor = _executor;
        require(executor != address(0), "Executor deployment failed");

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
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, address(this), params
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
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, address(this), params
        );

        vm.expectRevert(ERR_PROFIT_GATE);
        vm.prank(address(mockPool));
        executor.call(callData);
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
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, address(this), params
        );

        vm.expectRevert(ERR_ATOMIC_FAIL);
        vm.prank(address(mockPool));
        executor.call(callData);
    }

    // ─── 4. EIP-7702 Compatibility ─────────────────────────────────────────────
    function testEIP7702Compatibility() public {
        address eoa = makeAddr("eoa_wallet");
        string memory path = string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json");
        string memory json = vm.readFile(path);
        bytes memory code = vm.parseJsonBytes(json, ".deployedBytecode.object");
        vm.etch(eoa, code);

        bytes memory setPoolData = abi.encodeWithSelector(SEL_SET_POOL, address(mockPool));
        (bool ok, ) = eoa.call(setPoolData);
        require(ok, "setPool on EOA failed");

        bytes memory params = _getParams();
        mockTokenA.mint(eoa, 2000 ether);

        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, address(this), params
        );

        vm.prank(address(mockPool));
        (bool success, bytes memory ret) = eoa.call(callData);
        assertTrue(success, "EIP-7702 EOA should handle executeOperation");
        assertEq(abi.decode(ret, (bool)), true, "should return true");
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

}
