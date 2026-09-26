// ExprFuzz.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports ExprFuzz from the combined project file.
import "./_eq_658_project.sol";


// _eq_658_project.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

contract ExprFuzz {
    uint64 public m_count;

    constructor() payable {
        uint64 next_m_count = 0;

        m_count = next_m_count;
    }

    function probe(uint64 n) external returns (uint64) {
        uint64 x = uint64((m_count == 658));
        return m_count;
    }

}

