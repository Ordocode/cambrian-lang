// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports StdCryptoMatrix from the combined project file.
import "./_std-crypto-matrix_project.sol";

---
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

interface ICambrianFactory {
    function deployStdCryptoMatrix() external payable returns (address);
    function predictStdCryptoMatrix() external view returns (address);
}

contract StdCryptoMatrix {
    address public immutable _factory;
    uint64 public m_x;

    constructor(address factory_) payable {
        _factory = factory_;
        uint64 next_m_x = 0;

        m_x = next_m_x;
    }

    function runStdSha256() external returns (bytes memory) {
        return sha256(bytes("abc"));
    }

    function runEvmSha256() external returns (uint256) {
        return uint256(sha256(abi.encodePacked("abc")));
    }

}

contract CambrianFactory {
    event Deployed(string entityType, address instance);

    address public owner;
    mapping(address => bool) public isDeployed;

    constructor() {
        owner = msg.sender;
    }

    function deployStdCryptoMatrix() external payable returns (address) {
        require(msg.sender == owner || isDeployed[msg.sender], "CambrianFactory: unauthorized");
        StdCryptoMatrix _instance = new StdCryptoMatrix{salt: bytes32(0), value: msg.value}(address(this));
        isDeployed[address(_instance)] = true;
        emit Deployed("StdCryptoMatrix", address(_instance));
        return address(_instance);
    }

    function predictStdCryptoMatrix() external view returns (address) {
        return address(uint160(uint256(keccak256(abi.encodePacked(
            bytes1(0xff), address(this), bytes32(0),
            keccak256(abi.encodePacked(type(StdCryptoMatrix).creationCode, abi.encode(address(this))))
        )))));
    }

}

