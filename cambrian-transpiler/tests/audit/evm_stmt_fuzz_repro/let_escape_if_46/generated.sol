// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

contract StmtFuzz {
    uint64 public m_count;

    constructor() payable {
        uint64 next_m_count = 0;

        m_count = next_m_count;
    }

    function check(bool flag) external returns (uint64) {
        if (flag) {
            uint256 y = 5;
        }
        return uint64(y);
    }

}



// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports StmtFuzz from the combined project file.
import "./_let_escape_if_46_project.sol";
