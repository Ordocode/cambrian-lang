// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

interface IOracle {
    function isAlive() external view returns (bool);
}

contract Caller {
    uint64 public immutable m_id;
    address public m_oracle;

    constructor(uint64 m_id_, address oracle) payable {
        m_id = m_id_;
        address next_m_oracle = oracle;

        m_oracle = next_m_oracle;
    }

    function check(bool flag) external returns (bool) {
        // Phase: fetch
        {
            if (flag) {
                bool alive = IOracle(m_oracle).isAlive();
            }
            return alive;
        }
    }

}



// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports Caller from the combined project file.
import "./_T-EVM-ST-002_project.sol";
