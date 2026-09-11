// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// Time-dependent test contract for hanvil: nothing here can be exercised without moving the
/// chain's clock, which is the point. `sweep()` before `openUntil` reverts.
contract Deadline {
    uint256 public immutable openUntil;

    event Locked(uint256 until);
    event Expired(uint256 at);

    constructor(uint256 window) {
        openUntil = block.timestamp + window;
        emit Locked(openUntil);
    }

    function sweep() external {
        require(block.timestamp >= openUntil, "Deadline: too early");
        emit Expired(block.timestamp);
    }
}
