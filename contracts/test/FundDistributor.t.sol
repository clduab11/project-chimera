// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Test} from "forge-std/Test.sol";
import {FundDistributor} from "../src/FundDistributor.sol";

contract FundDistributorTest is Test {
    FundDistributor distributor;
    address owner = address(0xABCD);
    address worker1 = address(0x1111);
    address worker2 = address(0x2222);

    function setUp() public {
        vm.prank(owner);
        distributor = new FundDistributor(owner);
    }

    function test_Distribute_Success() public {
        vm.deal(owner, 1 ether);
        FundDistributor.Payment[] memory payments = new FundDistributor.Payment[](2);
        payments[0] = FundDistributor.Payment(payable(worker1), 0.1 ether);
        payments[1] = FundDistributor.Payment(payable(worker2), 0.2 ether);

        vm.prank(owner);
        distributor.distribute{value: 0.3 ether}(payments);

        assertEq(worker1.balance, 0.1 ether);
        assertEq(worker2.balance, 0.2 ether);
    }

    function test_Revert_ZeroPayment() public {
        vm.deal(owner, 1 ether);
        FundDistributor.Payment[] memory payments = new FundDistributor.Payment[](1);
        payments[0] = FundDistributor.Payment(payable(worker1), 0);

        vm.prank(owner);
        vm.expectRevert(FundDistributor.ZeroPayment.selector);
        distributor.distribute{value: 1 ether}(payments);
    }

    function test_EmergencyWithdraw() public {
        vm.deal(address(distributor), 1 ether);
        vm.prank(owner);
        distributor.emergencyWithdraw();
        assertEq(owner.balance, 1 ether);
    }
}
