// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/// @title FundDistributor
/// @notice Batch-send native ETH from a treasury to worker EOAs.
/// @dev Designed for L2 gas top-ups. Owner is typically a multisig.
contract FundDistributor {
    address public owner;
    address public pendingOwner;

    struct Payment {
        address payable to;
        uint256 amount;
    }

    event Distributed(uint256 count, uint256 totalValue);
    event OwnerTransferRequested(address indexed pendingOwner);
    event OwnerTransferred(address indexed oldOwner, address indexed newOwner);
    event EmergencyWithdraw(address indexed to, uint256 amount);

    error Unauthorized();
    error InsufficientBalance(uint256 required, uint256 available);
    error TransferFailed(address recipient);
    error ZeroPayment();
    error ZeroAddress();

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    constructor(address _owner) {
        if (_owner == address(0)) revert ZeroAddress();
        owner = _owner;
    }

    /// @notice Send ETH to multiple recipients atomically.
    /// @param payments Array of (recipient, amount) tuples.
    function distribute(Payment[] calldata payments) external payable onlyOwner {
        uint256 totalRequired;
        for (uint256 i = 0; i < payments.length; i++) {
            if (payments[i].amount == 0) revert ZeroPayment();
            totalRequired += payments[i].amount;
        }
        if (totalRequired > msg.value) {
            revert InsufficientBalance(totalRequired, msg.value);
        }

        for (uint256 i = 0; i < payments.length; i++) {
            (bool ok, ) = payments[i].to.call{value: payments[i].amount}("");
            if (!ok) revert TransferFailed(payments[i].to);
        }

        // Refund excess to caller
        uint256 excess = msg.value - totalRequired;
        if (excess > 0) {
            (bool ok, ) = msg.sender.call{value: excess}("");
            if (!ok) revert TransferFailed(msg.sender);
        }

        emit Distributed(payments.length, totalRequired);
    }

    /// @notice Two-step ownership transfer (prevents accidental loss).
    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        pendingOwner = newOwner;
        emit OwnerTransferRequested(newOwner);
    }

    function acceptOwnership() external {
        if (msg.sender != pendingOwner) revert Unauthorized();
        emit OwnerTransferred(owner, msg.sender);
        owner = pendingOwner;
        pendingOwner = address(0);
    }

    /// @notice Emergency drain. Only owner.
    function emergencyWithdraw() external onlyOwner {
        uint256 bal = address(this).balance;
        (bool ok, ) = owner.call{value: bal}("");
        if (!ok) revert TransferFailed(owner);
        emit EmergencyWithdraw(owner, bal);
    }

    receive() external payable {}
}
