// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {IAavePool} from "../src/interfaces/IAavePool.sol";
import {IDexRouter} from "../src/interfaces/IDexRouter.sol";

contract MockERC20 {
    mapping(address => uint256) public balanceOf;
    function mint(address to, uint256 amount) public { balanceOf[to] += amount; }
    function approve(address, uint256) public returns (bool) { return true; }
    function transfer(address, uint256) public returns (bool) { return true; }
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
        // Give the caller their collateral
        collateralToken.mint(msg.sender, 700 ether); // COLLATERAL_BAL
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

    bytes4 constant ERR_PROFIT_GATE = 0x2e5a0d02;
    bytes4 constant ERR_ATOMIC_FAIL = 0x5fe2e75c;
    bytes4 constant ERR_INVALID_ROUTER = 0x8d4f59a9;
    bytes4 constant SEL_EXECUTE_OPERATION = 0x1b11d0ff;
    bytes4 constant SEL_EXEC = 0x55f86501;
    bytes32 constant EVT_PROFIT_TOPIC0 = 0x357d905f1831209797df4d55d79c5c5bf1d9f7311c976afd05e13d881eab9bc8;

    uint256 constant FLASH_AMOUNT = 1000 ether;
    uint256 constant FLASH_PREMIUM = 5 ether;
    uint256 constant DEBT_TO_COVER = 800 ether;
    uint256 constant COLLATERAL_BAL = 700 ether;
    uint256 constant SWAP_OUT = 1200 ether;
    uint256 constant AMOUNT_OUT_MIN = 500 ether;
    uint256 constant MIN_PROFIT = 100 ether;
    uint256 constant TIP = 10 ether;
    uint256 constant DEADLINE = 2000000000;

    function setUp() public {
        mockPool = new MockPool();
        mockTokenA = new MockERC20();
        mockTokenB = new MockERC20();
        mockRouter = new MockRouter();
        
        mockPool.setExpectations(victim, mockTokenB, mockTokenA);
        mockRouter.setTokens(mockTokenB, mockTokenA);

        string memory path = string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json");
        string memory json = vm.readFile(path);
        bytes memory code = vm.parseJsonBytes(json, ".bytecode.object");
        address _executor;
        assembly {
            _executor := create(0, add(code, 0x20), mload(code))
        }
        executor = _executor;
        require(executor != address(0), "Executor deployment failed");
    }

    function _getParams() internal view returns (bytes memory) {
        return abi.encode(
            address(mockTokenB), victim, DEBT_TO_COVER, false, address(mockRouter),
            AMOUNT_OUT_MIN, MIN_PROFIT, TIP, DEADLINE
        );
    }

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

    function testAtomicRevertOnLiquidationFailure() public {
        bytes memory params = _getParams();
        mockTokenA.mint(executor, 2000 ether);

        // Make the mockPool revert on liquidationCall
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

    function testEIP7702Compatibility() public {
        address eoa = makeAddr("eoa_wallet");
        string memory path = string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json");
        string memory json = vm.readFile(path);
        // NOTE: For EIP-7702 delegation, we should use deployedBytecode, but for simplicity
        // in Foundry etching we can just etch the runtime code directly if we had it, but etching creation code won't work perfectly.
        // Actually, we'll just etch the creation code and it'll fail if we don't fix it. Let's use deployedBytecode.object.
        bytes memory code = vm.parseJsonBytes(json, ".deployedBytecode.object");
        vm.etch(eoa, code);

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

    function testDirectExecPath() public {
        bytes memory strategyData = abi.encode(
            address(mockTokenA), FLASH_AMOUNT, address(mockPool), address(mockTokenB),
            victim, DEBT_TO_COVER, false, address(mockRouter), AMOUNT_OUT_MIN, MIN_PROFIT, TIP, DEADLINE
        );
        mockTokenA.mint(executor, 2000 ether);

        bytes memory callData = abi.encodeWithSelector(SEL_EXEC, strategyData);

        vm.recordLogs();
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
}
