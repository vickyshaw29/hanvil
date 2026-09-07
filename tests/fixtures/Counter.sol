// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// Test contract for hanvil: state, an event, a custom error, a string revert, and a value sink.
contract Counter {
    uint256 public count;

    event Incremented(address indexed by, uint256 newCount);

    error TooHigh(uint256 requested, uint256 limit);

    function increment() external {
        count += 1;
        emit Incremented(msg.sender, count);
    }

    function incrementBy(uint256 n) external {
        if (n > 100) revert TooHigh(n, 100);
        count += n;
        emit Incremented(msg.sender, count);
    }

    function fail() external pure {
        revert("Counter: fail");
    }

    receive() external payable {}
}
