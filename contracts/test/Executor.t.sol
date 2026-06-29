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

    // ─── 6. Owner View ─────────────────────────────────────────────────────────
    function testOwnerView() public {
        bytes memory callData = abi.encodeWithSelector(SEL_OWNER);
        (bool ok, bytes memory data) = executor.staticcall(callData);
        assertTrue(ok, "owner() should succeed");
        address owner = abi.decode(data, (address));
        assertEq(owner, address(this), "owner should be address(this)");
    }

    // ─── 7. Unauthorized exec Reverts ──────────────────────────────────────────
    function testUnauthorizedExecReverts() public {
        address unauthorized = makeAddr("unauthorized_exec");
        bytes memory strategyData = abi.encode(
            address(mockTokenA), FLASH_AMOUNT, address(mockPool), address(mockTokenB),
            victim, DEBT_TO_COVER, false, address(mockRouter), AMOUNT_OUT_MIN, MIN_PROFIT, TIP, DEADLINE
        );

        bytes memory callData = abi.encodeWithSelector(SEL_EXEC, strategyData);

        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(unauthorized);
        executor.call(callData);
    }

    // ─── 8. Unauthorized setPool Reverts ───────────────────────────────────────
    function testUnauthorizedSetPoolReverts() public {
        address unauthorized = makeAddr("unauthorized_setpool");
        bytes memory callData = abi.encodeWithSelector(SEL_SET_POOL, address(mockPool));

        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(unauthorized);
        executor.call(callData);
    }

    // ─── 9. Unauthorized withdraw Reverts ──────────────────────────────────────
    function testUnauthorizedWithdrawReverts() public {
        address unauthorized = makeAddr("unauthorized_withdraw");
        bytes memory callData = abi.encodeWithSelector(SEL_WITHDRAW, address(mockTokenA), uint256(0));

        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(unauthorized);
        executor.call(callData);
    }

    // ─── 10. Invalid Pool Reverts ──────────────────────────────────────────────
    function testInvalidPoolReverts() public {
        address fakePool = makeAddr("fake_pool");
        bytes memory params = _getParams();
        mockTokenA.mint(executor, 2000 ether);

        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, address(this), params
        );

        vm.expectRevert(ERR_INVALID_POOL);
        vm.prank(fakePool);
        executor.call(callData);
    }

    // ─── 11. Withdraw ERC20 (full balance) ─────────────────────────────────────
    function testWithdrawERC20() public {
        uint256 amount = 500 ether;
        mockTokenA.mint(executor, amount);
        assertEq(mockTokenA.balanceOf(executor), amount, "executor should have tokens");

        bytes memory callData = abi.encodeWithSelector(SEL_WITHDRAW, address(mockTokenA), uint256(0));
        vm.prank(address(this));
        (bool ok, ) = executor.call(callData);
        assertTrue(ok, "withdraw should succeed");

        assertEq(mockTokenA.balanceOf(executor), 0, "executor balance should be zero");
        assertEq(mockTokenA.balanceOf(address(this)), amount, "owner should have tokens");
    }

    // ─── 12. Withdraw ETH ──────────────────────────────────────────────────────
    function testWithdrawETH() public {
        uint256 amount = 1 ether;
        vm.deal(executor, amount);
        assertEq(address(executor).balance, amount, "executor should have ETH");

        uint256 ownerBalBefore = address(this).balance;
        bytes memory callData = abi.encodeWithSelector(SEL_WITHDRAW, address(0), uint256(0));
        vm.prank(address(this));
        (bool ok, ) = executor.call(callData);
        assertTrue(ok, "ETH withdraw should succeed");

        assertEq(address(executor).balance, 0, "executor ETH balance should be zero");
        assertEq(address(this).balance - ownerBalBefore, amount, "owner should have received ETH");
    }

    // ─── 13. Withdraw ERC20 Specific Amount ─────────────────────────────────────
    function testWithdrawERC20SpecificAmount() public {
        uint256 totalAmount    = 500 ether;
        uint256 withdrawAmount = 200 ether;
        mockTokenA.mint(executor, totalAmount);

        bytes memory callData = abi.encodeWithSelector(SEL_WITHDRAW, address(mockTokenA), withdrawAmount);
        vm.prank(address(this));
        (bool ok, ) = executor.call(callData);
        assertTrue(ok, "withdraw specific amount should succeed");

        assertEq(mockTokenA.balanceOf(executor), totalAmount - withdrawAmount, "executor remaining balance wrong");
        assertEq(mockTokenA.balanceOf(address(this)), withdrawAmount, "owner should have received exact amount");
    }

    // ─── 14. Profit Gate Overflow Protection ────────────────────────────────────
    function testProfitGateOverflowProtection() public {
        bytes memory params = abi.encode(
            address(mockTokenB), victim, DEBT_TO_COVER, false, address(mockRouter),
            AMOUNT_OUT_MIN, type(uint256).max - 1, TIP, DEADLINE
        );
        mockTokenA.mint(executor, 2000 ether);

        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, address(this), params
        );

        vm.expectRevert(ERR_PROFIT_GATE);
        vm.prank(address(mockPool));
        executor.call(callData);
    }

    // ─── 15. SetPool Can Be Updated ─────────────────────────────────────────────
    function testSetPoolCanBeUpdated() public {
        MockPool newPool = new MockPool();
        newPool.setExpectations(victim, mockTokenB, mockTokenA);

        bytes memory setPoolData = abi.encodeWithSelector(SEL_SET_POOL, address(newPool));
        vm.prank(address(this));
        (bool ok, ) = executor.call(setPoolData);
        assertTrue(ok, "setPool update should succeed");

        bytes memory params = _getParams();
        mockTokenA.mint(executor, 2000 ether);
        bytes memory callData = abi.encodeWithSelector(
            SEL_EXECUTE_OPERATION, address(mockTokenA), FLASH_AMOUNT, FLASH_PREMIUM, address(this), params
        );

        vm.expectRevert(ERR_INVALID_POOL);
        vm.prank(address(mockPool));
        executor.call(callData);

        vm.prank(address(newPool));
        (bool success, bytes memory ret) = executor.call(callData);
        assertTrue(success, "new pool executeOperation should succeed");
        assertEq(abi.decode(ret, (bool)), true, "should return true");
    }

    // ─── Helper: deploy a fresh Executor, optionally appending a 32-byte owner arg ─
    function _deployWithOwnerArg(address ownerArg, bool appendArg) internal returns (address dep) {
        string memory path = string.concat(vm.projectRoot(), "/out/Executor.yul/Executor.json");
        string memory json = vm.readFile(path);
        bytes memory code  = vm.parseJsonBytes(json, ".bytecode.object");
        // The constructor reads an OPTIONAL appended 32-byte address arg. Appending
        // abi.encode(owner) (a left-padded 32-byte word) sets owner (slot 0) at
        // construction time. With appendArg == false we deploy raw creation code, so
        // the runtime lazy-init path (first caller becomes owner) remains active.
        bytes memory initCode = appendArg ? abi.encodePacked(code, abi.encode(ownerArg)) : code;
        assembly {
            dep := create(0, add(initCode, 0x20), mload(initCode))
        }
        require(dep != address(0), "deploy failed");
    }

    // ─── 16. Constructor sets owner from appended arg ───────────────────────────
    function testConstructorSetsOwnerFromArg() public {
        address desiredOwner = makeAddr("multisig_owner");
        address dep = _deployWithOwnerArg(desiredOwner, true);

        // owner() should return desiredOwner WITHOUT any prior state-changing call,
        // because the constructor wrote slot 0. Since slot 0 != 0, owner() does NOT
        // attempt a lazy-init sstore, so a staticcall is safe here.
        (bool ok, bytes memory data) = dep.staticcall(abi.encodeWithSelector(SEL_OWNER));
        assertTrue(ok, "owner() should succeed");
        assertEq(abi.decode(data, (address)), desiredOwner, "constructor owner mismatch");
    }

    // ─── 17. Constructor owner blocks lazy-init override ────────────────────────
    function testConstructorOwnerArgBlocksLazyInit() public {
        address desiredOwner = makeAddr("multisig_owner_2");
        address dep = _deployWithOwnerArg(desiredOwner, true);

        // A DIFFERENT caller must NOT be able to hijack ownership via lazy-init.
        address attacker = makeAddr("lazy_attacker");
        bytes memory strategyData = abi.encode(
            address(mockTokenA), FLASH_AMOUNT, address(mockPool), address(mockTokenB),
            victim, DEBT_TO_COVER, false, address(mockRouter), AMOUNT_OUT_MIN, MIN_PROFIT, TIP, DEADLINE
        );
        bytes memory callData = abi.encodeWithSelector(SEL_EXEC, strategyData);

        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(attacker);
        dep.call(callData);
    }

    // ─── 18. transferOwnership happy path ───────────────────────────────────────
    function testTransferOwnership() public {
        // setUp made address(this) the owner (first setPool caller).
        address newOwner = makeAddr("new_owner");

        bytes memory callData = abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, newOwner);
        vm.prank(address(this));
        (bool ok, ) = executor.call(callData);
        assertTrue(ok, "transferOwnership should succeed");

        (bool ok2, bytes memory data) = executor.staticcall(abi.encodeWithSelector(SEL_OWNER));
        assertTrue(ok2, "owner() should succeed");
        assertEq(abi.decode(data, (address)), newOwner, "owner should be newOwner");
    }

    // ─── 19. transferOwnership only owner ───────────────────────────────────────
    function testTransferOwnershipOnlyOwner() public {
        address attacker = makeAddr("transfer_attacker");
        bytes memory callData = abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, attacker);

        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(attacker);
        executor.call(callData);

        // owner unchanged
        (bool ok, bytes memory data) = executor.staticcall(abi.encodeWithSelector(SEL_OWNER));
        assertTrue(ok, "owner() should succeed");
        assertEq(abi.decode(data, (address)), address(this), "owner should be unchanged");
    }

    // ─── 20. transferOwnership rejects zero address ─────────────────────────────
    function testTransferOwnershipRejectsZeroAddress() public {
        // Critical: a zero owner would re-enable lazy-init hijacking.
        bytes memory callData = abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, address(0));

        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(address(this));
        executor.call(callData);

        // owner unchanged
        (bool ok, bytes memory data) = executor.staticcall(abi.encodeWithSelector(SEL_OWNER));
        assertTrue(ok, "owner() should succeed");
        assertEq(abi.decode(data, (address)), address(this), "owner should be unchanged");
    }

    // ─── 21. Transferred owner can call gated functions ─────────────────────────
    function testTransferredOwnerCanCallGatedFns() public {
        address newOwner = makeAddr("new_gated_owner");

        bytes memory transferData = abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, newOwner);
        vm.prank(address(this));
        (bool ok, ) = executor.call(transferData);
        assertTrue(ok, "transferOwnership should succeed");

        // New owner can call setPool.
        MockPool somePool = new MockPool();
        bytes memory setPoolData = abi.encodeWithSelector(SEL_SET_POOL, address(somePool));
        vm.prank(newOwner);
        (bool ok2, ) = executor.call(setPoolData);
        assertTrue(ok2, "new owner setPool should succeed");

        // Old owner can no longer call setPool.
        vm.expectRevert(ERR_UNAUTHORIZED);
        vm.prank(address(this));
        executor.call(setPoolData);
    }

    // ─── 22. Backward compat: no-arg deploy uses lazy-init ──────────────────────
    function testBackwardCompatNoArgDeployLazyInit() public {
        // Deploy with NO constructor arg (raw creation code, like setUp).
        address dep = _deployWithOwnerArg(address(0), false);
        address addrX = makeAddr("first_caller");

        // SUBTLETY: under lazy-init, when slot 0 == 0 the runtime performs an sstore
        // (owner := caller) before dispatch. owner() therefore triggers that sstore
        // on the FIRST call, so a staticcall to owner() here would REVERT (no SSTORE
        // allowed in a static context). We make the first call a normal (state-
        // changing) call — setPool from addrX — which establishes addrX as owner.
        MockPool somePool = new MockPool();
        bytes memory setPoolData = abi.encodeWithSelector(SEL_SET_POOL, address(somePool));
        vm.prank(addrX);
        (bool ok, ) = dep.call(setPoolData);
        assertTrue(ok, "first caller setPool (lazy-init) should succeed");

        // Now slot 0 is set, so owner() no longer needs to sstore → staticcall is safe.
        (bool ok2, bytes memory data) = dep.staticcall(abi.encodeWithSelector(SEL_OWNER));
        assertTrue(ok2, "owner() should succeed");
        assertEq(abi.decode(data, (address)), addrX, "first caller should be owner via lazy-init");
    }
}
