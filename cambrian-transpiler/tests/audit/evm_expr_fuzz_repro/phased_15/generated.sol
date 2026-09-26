// PhasedExpr.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports PhasedExpr from the combined project file.
import "./_phased_15_project.sol";


// _phased_15_project.sol
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

contract PhasedExpr {
    uint64 public m_a;
    uint64 public m_b;

    constructor() payable {
        m_a = 0;
        m_b = 0;
    }

    function bump(uint64 amount) external {
        // Phase: inc
        {
            uint64 next_m_a = (m_a + amount);
    
            m_a = next_m_a;
        }
        // Phase: mirror
        {
            uint64 next_m_b = uint64(next_m_a);
    
            m_b = next_m_b;
        }
    }

}

