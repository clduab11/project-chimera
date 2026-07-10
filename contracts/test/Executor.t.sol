// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";

contract MockERC20 {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function transferFrom(
        address from,
        address to,
        uint256 amount
    ) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "allowance");
        allowance[from][msg.sender] = allowed - amount;
        _transfer(from, to, amount);
        return true;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        require(balanceOf[from] >= amount, "balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
    }
}

contract MockPool {
    bytes4 internal constant SEL_EXECUTE_OPERATION = 0x1b11d0ff;

    address public expectedExecutor;
    address public expectedVictim;
    MockERC20 public collateralToken;
    MockERC20 public debtToken;
    uint256 public premium = 5 ether;
    uint256 public collateralToMint = 700 ether;
    bool public failFlashLoan;

    bool public flashLoanCalled;
    address public flashLoanCaller;
    address public flashLoanReceiver;
    address public callbackInitiator;
    uint256 public repaidAmount;
    bytes32 public callbackParamsHash;

    function setExpectations(
        address executor,
        address victim,
        MockERC20 collateral,
        MockERC20 debt
    ) external {
        expectedExecutor = executor;
        expectedVictim = victim;
        collateralToken = collateral;
        debtToken = debt;
    }

    function setFailFlashLoan(bool fail) external {
        failFlashLoan = fail;
    }

    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external {
        if (failFlashLoan) {
            assembly {
                revert(0, 0)
            }
        }

        require(msg.sender == expectedExecutor, "executor did not initiate");
        require(receiverAddress == expectedExecutor, "wrong receiver");
        require(asset == address(debtToken), "wrong flash asset");
        require(params.length == 288, "wrong callback params length");
        require(referralCode == 0, "wrong referral");

        flashLoanCalled = true;
        flashLoanCaller = msg.sender;
        flashLoanReceiver = receiverAddress;
        callbackInitiator = msg.sender;
        callbackParamsHash = keccak256(params);

        debtToken.mint(receiverAddress, amount);
        (bool ok, bytes memory result) = receiverAddress.call(
            abi.encodeWithSelector(
                SEL_EXECUTE_OPERATION,
                asset,
                amount,
                premium,
                msg.sender,
                params
            )
        );
        if (!ok) {
            assembly {
                revert(add(result, 32), mload(result))
            }
        }
        require(abi.decode(result, (bool)), "callback returned false");

        repaidAmount = amount + premium;
        require(
            debtToken.transferFrom(receiverAddress, address(this), repaidAmount),
            "repayment failed"
        );
    }

    function liquidationCall(
        address collateralAsset,
        address debtAsset,
        address user,
        uint256 debtToCover,
        bool receiveAToken
    ) external {
        require(msg.sender == expectedExecutor, "wrong liquidator");
        require(collateralAsset == address(collateralToken), "wrong collateral");
        require(debtAsset == address(debtToken), "wrong debt asset");
        require(user == expectedVictim, "wrong victim");
        require(!receiveAToken, "unexpected aToken mode");
        require(
            debtToken.transferFrom(msg.sender, address(this), debtToCover),
            "debt transfer failed"
        );
        collateralToken.mint(msg.sender, collateralToMint);
    }
}

contract MockRouter {
    MockERC20 public tokenIn;
    MockERC20 public tokenOut;
    uint256 public amountToMint = 1200 ether;

    function setTokens(MockERC20 input, MockERC20 output) external {
        tokenIn = input;
        tokenOut = output;
    }

    function setAmountToMint(uint256 amount) external {
        amountToMint = amount;
    }

    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts) {
        require(path.length == 2, "path length");
        require(path[0] == address(tokenIn), "wrong token in");
        require(path[1] == address(tokenOut), "wrong token out");
        require(amountToMint >= amountOutMin, "slippage");
        require(deadline != 0, "deadline");
        require(
            tokenIn.transferFrom(msg.sender, address(this), amountIn),
            "input transfer"
        );
        tokenOut.mint(to, amountToMint);

        amounts = new uint256[](2);
        amounts[0] = amountIn;
        amounts[1] = amountToMint;
    }
}

contract ExecutorTest is Test {
    bytes4 internal constant SEL_EXECUTE = 0x09c5eabe;
    bytes4 internal constant SEL_LEGACY_EXEC = 0x55f86501;
    bytes4 internal constant SEL_EXECUTE_OPERATION = 0x1b11d0ff;
    bytes4 internal constant SEL_OWNER = 0x8da5cb5b;
    bytes4 internal constant SEL_POOL = 0x16f0115b;
    bytes4 internal constant SEL_SET_POOL = 0x4437152a;
    bytes4 internal constant SEL_SET_WORKER = 0xc373d7f3;
    bytes4 internal constant SEL_IS_WORKER = 0xaa156645;
    bytes4 internal constant SEL_WITHDRAW = 0xf3fef3a3;
    bytes4 internal constant SEL_TRANSFER_OWNERSHIP = 0xf2fde38b;

    bytes4 internal constant ERR_PROFIT_GATE = 0x9b89663c;
    bytes4 internal constant ERR_ATOMIC_FAIL = 0xc4cae92f;
    bytes4 internal constant ERR_UNAUTHORIZED = 0x82b42900;
    bytes4 internal constant ERR_INVALID_POOL = 0x2083cd40;

    bytes32 internal constant EVT_PROFIT_TOPIC0 =
        0x357d905f1831209797df4d55d79c5c5bf1d9f7311c976afd05e13d881eab9bc8;

    uint256 internal constant FLASH_AMOUNT = 1000 ether;
    uint256 internal constant FLASH_PREMIUM = 5 ether;
    uint256 internal constant DEBT_TO_COVER = 800 ether;
    uint256 internal constant AMOUNT_OUT_MIN = 500 ether;
    uint256 internal constant MIN_PROFIT = 100 ether;
    uint256 internal constant TIP = 10 ether;
    uint256 internal constant DEADLINE = 2_000_000_000;

    address internal executor;
    address internal worker;
    address internal victim;
    MockPool internal mockPool;
    MockERC20 internal debtToken;
    MockERC20 internal collateralToken;
    MockRouter internal mockRouter;

    receive() external payable {}

    function setUp() public {
        worker = makeAddr("worker");
        victim = makeAddr("victim");
        mockPool = new MockPool();
        debtToken = new MockERC20();
        collateralToken = new MockERC20();
        mockRouter = new MockRouter();
        mockRouter.setTokens(collateralToken, debtToken);

        executor = _deployExecutor(address(this));
        mockPool.setExpectations(executor, victim, collateralToken, debtToken);
        _ownerCall(abi.encodeWithSelector(SEL_SET_POOL, address(mockPool)));
    }

    function testAuthorizedWorkerExecutesFullFlashLoanFlow() public {
        _setWorker(worker, true);
        bytes memory request = _request();
        bytes memory params = _params();

        vm.recordLogs();
        vm.prank(worker);
        (bool ok, bytes memory result) = executor.call(
            abi.encodeWithSelector(SEL_EXECUTE, request)
        );

        assertTrue(ok, _revertMessage(result));
        assertTrue(mockPool.flashLoanCalled(), "flash loan not called");
        assertEq(mockPool.flashLoanCaller(), executor, "Executor must initiate");
        assertEq(mockPool.flashLoanReceiver(), executor, "Executor must receive");
        assertEq(mockPool.callbackInitiator(), executor, "wrong Aave initiator");
        assertEq(mockPool.callbackParamsHash(), keccak256(params), "params changed");
        assertEq(mockPool.repaidAmount(), FLASH_AMOUNT + FLASH_PREMIUM, "repayment");
        assertEq(
            debtToken.allowance(executor, address(mockPool)),
            0,
            "repayment allowance must be consumed"
        );
        assertEq(debtToken.balanceOf(executor), 395 ether, "net profit balance");
        assertEq(
            debtToken.balanceOf(address(mockPool)),
            DEBT_TO_COVER + FLASH_AMOUNT + FLASH_PREMIUM,
            "pool did not pull debt and repayment"
        );
        assertEq(
            collateralToken.balanceOf(address(mockRouter)),
            700 ether,
            "router did not pull collateral"
        );

        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool foundProfit;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == executor && logs[i].topics[0] == EVT_PROFIT_TOPIC0) {
                assertEq(abi.decode(logs[i].data, (uint256)), 395 ether, "profit event");
                foundProfit = true;
                break;
            }
        }
        assertTrue(foundProfit, "Profit event missing");
    }

    function testOwnerCanExecute() public {
        (bool ok, bytes memory result) = executor.call(
            abi.encodeWithSelector(SEL_EXECUTE, _request())
        );
        assertTrue(ok, _revertMessage(result));
        assertEq(mockPool.flashLoanCaller(), executor, "Executor must initiate");
    }

    function testUnauthorizedWorkerCannotExecute() public {
        vm.prank(worker);
        vm.expectRevert(ERR_UNAUTHORIZED);
        executor.call(abi.encodeWithSelector(SEL_EXECUTE, _request()));
        assertFalse(mockPool.flashLoanCalled(), "pool must not be called");
    }

    function testLegacyDirectExecSelectorIsRemoved() public {
        vm.expectRevert(ERR_UNAUTHORIZED);
        executor.call(abi.encodeWithSelector(SEL_LEGACY_EXEC, _request()));
    }

    function testExecuteRejectsWrongPayloadLengths() public {
        _setWorker(worker, true);
        bytes memory request = _request();

        vm.prank(worker);
        vm.expectRevert(ERR_ATOMIC_FAIL);
        executor.call(
            abi.encodeWithSelector(SEL_EXECUTE, _copyPrefix(request, request.length - 1))
        );

        vm.prank(worker);
        vm.expectRevert(ERR_ATOMIC_FAIL);
        executor.call(abi.encodeWithSelector(SEL_EXECUTE, abi.encodePacked(request, hex"00")));
    }

    function testExecuteRejectsZeroAssetAndAmount() public {
        _setWorker(worker, true);

        bytes memory zeroAsset = _request();
        assembly {
            mstore(add(zeroAsset, 32), 0)
        }
        vm.prank(worker);
        vm.expectRevert(ERR_ATOMIC_FAIL);
        executor.call(abi.encodeWithSelector(SEL_EXECUTE, zeroAsset));

        bytes memory zeroAmount = _request();
        assembly {
            mstore(add(zeroAmount, 64), 0)
        }
        vm.prank(worker);
        vm.expectRevert(ERR_ATOMIC_FAIL);
        executor.call(abi.encodeWithSelector(SEL_EXECUTE, zeroAmount));
    }

    function testExecuteRejectsUnconfiguredPool() public {
        _ownerCall(abi.encodeWithSelector(SEL_SET_POOL, address(0)));
        _setWorker(worker, true);

        vm.prank(worker);
        vm.expectRevert(ERR_INVALID_POOL);
        executor.call(abi.encodeWithSelector(SEL_EXECUTE, _request()));

        _ownerCall(abi.encodeWithSelector(SEL_SET_POOL, makeAddr("poolEOA")));
        vm.prank(worker);
        vm.expectRevert(ERR_INVALID_POOL);
        executor.call(abi.encodeWithSelector(SEL_EXECUTE, _request()));
    }

    function testFlashLoanFailureRevertsAtomically() public {
        _setWorker(worker, true);
        mockPool.setFailFlashLoan(true);

        vm.prank(worker);
        vm.expectRevert(ERR_ATOMIC_FAIL);
        executor.call(abi.encodeWithSelector(SEL_EXECUTE, _request()));
    }

    function testProfitGateStillRevertsFullFlow() public {
        _setWorker(worker, true);
        mockRouter.setAmountToMint(50 ether);

        vm.prank(worker);
        vm.expectRevert(ERR_PROFIT_GATE);
        executor.call(abi.encodeWithSelector(SEL_EXECUTE, _request()));
        assertFalse(mockPool.flashLoanCalled(), "revert must roll back Pool state");
        assertEq(debtToken.balanceOf(executor), 0, "revert must roll back token state");
    }

    function testExecuteOperationRejectsWrongCaller() public {
        vm.prank(makeAddr("notPool"));
        vm.expectRevert(ERR_INVALID_POOL);
        executor.call(
            abi.encodeWithSelector(
                SEL_EXECUTE_OPERATION,
                address(debtToken),
                FLASH_AMOUNT,
                FLASH_PREMIUM,
                executor,
                _params()
            )
        );
    }

    function testExecuteOperationRejectsWrongInitiator() public {
        vm.prank(address(mockPool));
        vm.expectRevert(ERR_UNAUTHORIZED);
        executor.call(
            abi.encodeWithSelector(
                SEL_EXECUTE_OPERATION,
                address(debtToken),
                FLASH_AMOUNT,
                FLASH_PREMIUM,
                worker,
                _params()
            )
        );
    }

    function testExecuteOperationRejectsWrongPayloadLengths() public {
        bytes memory params = _params();

        vm.prank(address(mockPool));
        vm.expectRevert(ERR_ATOMIC_FAIL);
        executor.call(
            abi.encodeWithSelector(
                SEL_EXECUTE_OPERATION,
                address(debtToken),
                FLASH_AMOUNT,
                FLASH_PREMIUM,
                executor,
                _copyPrefix(params, params.length - 1)
            )
        );

        vm.prank(address(mockPool));
        vm.expectRevert(ERR_ATOMIC_FAIL);
        executor.call(
            abi.encodeWithSelector(
                SEL_EXECUTE_OPERATION,
                address(debtToken),
                FLASH_AMOUNT,
                FLASH_PREMIUM,
                executor,
                abi.encodePacked(params, hex"00")
            )
        );
    }

    function testSetWorkerOnlyOwnerAndViewReflectsChanges() public {
        address stranger = makeAddr("stranger");
        vm.prank(stranger);
        vm.expectRevert(ERR_UNAUTHORIZED);
        executor.call(abi.encodeWithSelector(SEL_SET_WORKER, worker, true));

        assertFalse(_isWorker(worker), "worker starts authorized");
        _setWorker(worker, true);
        assertTrue(_isWorker(worker), "worker not authorized");
        assertEq(
            uint256(vm.load(executor, keccak256(abi.encode(worker, uint256(2))))),
            1,
            "unexpected worker storage slot"
        );
        _setWorker(worker, false);
        assertFalse(_isWorker(worker), "worker not revoked");
    }

    function testSetPoolOnlyOwner() public {
        address stranger = makeAddr("stranger");
        vm.prank(stranger);
        vm.expectRevert(ERR_UNAUTHORIZED);
        executor.call(abi.encodeWithSelector(SEL_SET_POOL, stranger));

        (bool ok, bytes memory data) = executor.staticcall(
            abi.encodeWithSelector(SEL_POOL)
        );
        assertTrue(ok, "pool view failed");
        assertEq(abi.decode(data, (address)), address(mockPool), "pool changed");
    }

    function testConstructionOwnerIsConfigured() public view {
        (bool ok, bytes memory data) = executor.staticcall(
            abi.encodeWithSelector(SEL_OWNER)
        );
        assertTrue(ok, "owner view failed");
        assertEq(abi.decode(data, (address)), address(this), "wrong owner");
    }

    function testWithdrawOnlyOwner() public {
        debtToken.mint(executor, 10 ether);
        vm.prank(worker);
        vm.expectRevert(ERR_UNAUTHORIZED);
        executor.call(
            abi.encodeWithSelector(SEL_WITHDRAW, address(debtToken), 1 ether)
        );

        _ownerCall(
            abi.encodeWithSelector(SEL_WITHDRAW, address(debtToken), 4 ether)
        );
        assertEq(debtToken.balanceOf(address(this)), 4 ether, "owner withdrawal");
    }

    function testWithdrawNativeByOwner() public {
        vm.deal(executor, 1 ether);
        uint256 balanceBefore = address(this).balance;

        _ownerCall(abi.encodeWithSelector(SEL_WITHDRAW, address(0), uint256(0)));

        assertEq(address(this).balance, balanceBefore + 1 ether, "native withdrawal");
        assertEq(executor.balance, 0, "Executor retained native balance");
    }

    function testTransferOwnershipOnlyOwner() public {
        address newOwner = makeAddr("newOwner");
        vm.prank(worker);
        vm.expectRevert(ERR_UNAUTHORIZED);
        executor.call(abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, newOwner));

        _ownerCall(abi.encodeWithSelector(SEL_TRANSFER_OWNERSHIP, newOwner));
        (bool ok, bytes memory data) = executor.staticcall(
            abi.encodeWithSelector(SEL_OWNER)
        );
        assertTrue(ok, "owner view failed");
        assertEq(abi.decode(data, (address)), newOwner, "ownership unchanged");
    }

    function testConstructorRejectsMissingOwnerArgument() public {
        bytes memory code = _executorCreationCode();
        address deployed;
        assembly {
            deployed := create(0, add(code, 32), mload(code))
        }
        assertEq(deployed, address(0), "bare deployment must fail");
    }

    function testRequestAndCallbackShapesAreExact() public view {
        bytes memory request = _request();
        bytes memory params = _params();
        assertEq(request.length, 352, "request must be eleven words");
        assertEq(params.length, 288, "params must be nine words");
        assertEq(
            keccak256(_copyRange(request, 64, 288)),
            keccak256(params),
            "callback params are not final nine words"
        );
    }

    function testSelectorsMatchCanonicalSignatures() public pure {
        assertEq(SEL_EXECUTE, bytes4(keccak256("execute(bytes)")));
        assertEq(
            SEL_EXECUTE_OPERATION,
            bytes4(
                keccak256(
                    "executeOperation(address,uint256,uint256,address,bytes)"
                )
            )
        );
        assertEq(SEL_POOL, bytes4(keccak256("pool()")));
        assertEq(SEL_SET_POOL, bytes4(keccak256("setPool(address)")));
        assertEq(SEL_SET_WORKER, bytes4(keccak256("setWorker(address,bool)")));
        assertEq(SEL_IS_WORKER, bytes4(keccak256("isWorker(address)")));
    }

    function _request() internal view returns (bytes memory) {
        return
            abi.encode(
                address(debtToken),
                FLASH_AMOUNT,
                address(collateralToken),
                victim,
                DEBT_TO_COVER,
                false,
                address(mockRouter),
                AMOUNT_OUT_MIN,
                MIN_PROFIT,
                TIP,
                DEADLINE
            );
    }

    function _params() internal view returns (bytes memory) {
        return
            abi.encode(
                address(collateralToken),
                victim,
                DEBT_TO_COVER,
                false,
                address(mockRouter),
                AMOUNT_OUT_MIN,
                MIN_PROFIT,
                TIP,
                DEADLINE
            );
    }

    function _setWorker(address account, bool enabled) internal {
        _ownerCall(abi.encodeWithSelector(SEL_SET_WORKER, account, enabled));
    }

    function _isWorker(address account) internal view returns (bool) {
        (bool ok, bytes memory data) = executor.staticcall(
            abi.encodeWithSelector(SEL_IS_WORKER, account)
        );
        require(ok, "isWorker failed");
        return abi.decode(data, (bool));
    }

    function _ownerCall(bytes memory data) internal {
        (bool ok, bytes memory result) = executor.call(data);
        require(ok, _revertMessage(result));
    }

    function _deployExecutor(address initialOwner) internal returns (address deployed) {
        bytes memory initCode = abi.encodePacked(
            _executorCreationCode(),
            abi.encode(initialOwner)
        );
        assembly {
            deployed := create(0, add(initCode, 32), mload(initCode))
        }
        require(deployed != address(0), "Executor deployment failed");
    }

    function _executorCreationCode() internal view returns (bytes memory) {
        string memory path = string.concat(
            vm.projectRoot(),
            "/out/Executor.yul/Executor.json"
        );
        return vm.parseJsonBytes(vm.readFile(path), ".bytecode.object");
    }

    function _copyPrefix(
        bytes memory data,
        uint256 length
    ) internal pure returns (bytes memory result) {
        return _copyRange(data, 0, length);
    }

    function _copyRange(
        bytes memory data,
        uint256 start,
        uint256 length
    ) internal pure returns (bytes memory result) {
        result = new bytes(length);
        for (uint256 i; i < length; ++i) {
            result[i] = data[start + i];
        }
    }

    function _revertMessage(bytes memory result) internal pure returns (string memory) {
        if (result.length == 0) return "call reverted without data";
        return "call reverted";
    }
}
