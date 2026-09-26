// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

interface ICambrianFactory {
    function deployStdStrMatrix() external payable returns (address);
    function predictStdStrMatrix() external view returns (address);
}

function _cam_try_parse_radix(string memory s, uint256 radix, uint256 maxBits) pure returns (bool ok, uint256 value) {
    bytes memory b = bytes(s);
    uint256 i = 0;
    uint256 base = radix;
    if (b.length >= 2 && b[0] == "0" && (b[1] == "x" || b[1] == "X")) {
        if (base != 0 && base != 16) return (false, 0);
        base = 16;
        i = 2;
    } else if (base == 0) {
        base = 10;
    }
    if (base < 2 || base > 36) return (false, 0);
    if (i >= b.length) return (false, 0);
    uint256 maxV = maxBits >= 256 ? type(uint256).max : ((uint256(1) << maxBits) - 1);
    uint256 n = 0;
    for (; i < b.length; ++i) {
        uint8 c = uint8(b[i]);
        uint256 digit;
        if (c >= 48 && c <= 57) {
            digit = uint256(c - 48);
        } else if (c >= 65 && c <= 90) {
            digit = uint256(c - 55);
        } else if (c >= 97 && c <= 122) {
            digit = uint256(c - 87);
        } else {
            return (false, 0);
        }
        if (digit >= base) return (false, 0);
        if (n > type(uint256).max / base) return (false, 0);
        uint256 scaled = n * base;
        if (digit > type(uint256).max - scaled) return (false, 0);
        uint256 next = scaled + digit;
        if (next > maxV) return (false, 0);
        n = next;
    }
    return (true, n);
}

function _cam_parse_radix_or_zero(string memory s, uint256 radix, uint256 maxBits) pure returns (uint256) {
    (bool ok, uint256 v) = _cam_try_parse_radix(s, radix, maxBits);
    return ok ? v : 0;
}
function _cam_try_parse_radix_signed(string memory s, uint256 radix, uint256 maxBits) pure returns (bool ok, int256 value) {
    bytes memory b = bytes(s);
    bool neg = false;
    uint256 i = 0;
    if (b.length > 0 && b[0] == "-") {
        neg = true;
        i = 1;
    }
    uint256 base = radix;
    if (b.length >= i + 2 && b[i] == "0" && (b[i + 1] == "x" || b[i + 1] == "X")) {
        if (base != 0 && base != 16) return (false, 0);
        base = 16;
        i += 2;
    } else if (base == 0) {
        base = 10;
    }
    if (base < 2 || base > 36) return (false, 0);
    if (i >= b.length) return (false, 0);
    uint256 maxPos = maxBits >= 256 ? type(uint256).max : ((uint256(1) << (maxBits - 1)) - 1);
    uint256 maxMag = neg ? (maxPos + 1) : maxPos;
    uint256 n = 0;
    for (; i < b.length; ++i) {
        uint8 c = uint8(b[i]);
        uint256 digit;
        if (c >= 48 && c <= 57) {
            digit = uint256(c - 48);
        } else if (c >= 65 && c <= 90) {
            digit = uint256(c - 55);
        } else if (c >= 97 && c <= 122) {
            digit = uint256(c - 87);
        } else {
            return (false, 0);
        }
        if (digit >= base) return (false, 0);
        uint256 next = n * base + digit;
        if (next > maxMag) return (false, 0);
        n = next;
    }
    int256 out = neg ? -int256(n) : int256(n);
    return (true, out);
}

function _cam_parse_radix_signed_or_zero(string memory s, uint256 radix, uint256 maxBits) pure returns (int256) {
    (bool ok, int256 v) = _cam_try_parse_radix_signed(s, radix, maxBits);
    return ok ? v : int256(0);
}
function _cam_itoa(int256 v) pure returns (string memory) {
    if (v == 0) return "0";
    bool neg = v < 0;
    uint256 n = uint256(neg ? -v : v);
    bytes memory tmp = new bytes(78);
    uint256 len = 0;
    while (n > 0) {
        tmp[len] = bytes1(uint8(48 + (n % 10)));
        n /= 10;
        unchecked { ++len; }
    }
    bytes memory out = new bytes(neg ? len + 1 : len);
    uint256 j = 0;
    if (neg) { out[0] = "-"; j = 1; }
    for (uint256 k = 0; k < len; ++k) {
        out[j + k] = tmp[len - 1 - k];
    }
    return string(out);
}
function _cam_format_pad6(uint256 v, uint256 width) pure returns (string memory) {
    string memory t = _cam_itoa(int256(v));
    bytes memory tb = bytes(t);
    if (tb.length >= width) return t;
  bytes memory out = new bytes(width);
  uint256 pad = width - tb.length;
  for (uint256 i = 0; i < pad; ++i) { out[i] = "0"; }
  for (uint256 j = 0; j < tb.length; ++j) { out[pad + j] = tb[j]; }
  return string(out);
}

contract StdStrMatrix {
    address public immutable _factory;
    uint64 public m_x;

    constructor(address factory_) payable {
        _factory = factory_;
        uint64 next_m_x = 0;

        m_x = next_m_x;
    }

    function runFormatDefault() external returns (string memory) {
        return _cam_itoa(int256(42));
    }

    function runFormatPad6() external returns (string memory) {
        return _cam_format_pad6(42, 6);
    }

    function runParseDec() external returns (uint64) {
        (bool _cam_tmp0, uint256 _cam_tmp1) = _cam_try_parse_radix("123", 10, 64);
        uint64 _cam_tmp2 = 0;
        if (_cam_tmp0) {
            uint64 x = uint64(_cam_tmp1);
            _cam_tmp2 = x;
        } else if (!_cam_tmp0) {
            _cam_tmp2 = 0;
        }
        uint64 v = _cam_tmp2;
        return v;
    }

    function runParseHex() external returns (uint64) {
        (bool _cam_tmp3, uint256 _cam_tmp4) = _cam_try_parse_radix("ff", 16, 64);
        uint64 _cam_tmp5 = 0;
        if (_cam_tmp3) {
            uint64 x = uint64(_cam_tmp4);
            _cam_tmp5 = x;
        } else if (!_cam_tmp3) {
            _cam_tmp5 = 0;
        }
        uint64 v = _cam_tmp5;
        return v;
    }

    function runParseU8Overflow() external returns (bool) {
        (bool _cam_tmp6, uint256 _cam_tmp7) = _cam_try_parse_radix("100", 16, 8);
        bool _cam_tmp8 = false;
        if (_cam_tmp6) {
            _cam_tmp8 = false;
        } else if (!_cam_tmp6) {
            _cam_tmp8 = true;
        }
        bool bad = _cam_tmp8;
        return bad;
    }

    function runParseU8Max() external returns (uint8) {
        (bool _cam_tmp9, uint256 _cam_tmp10) = _cam_try_parse_radix("255", 10, 8);
        uint8 _cam_tmp11 = 0;
        if (_cam_tmp9) {
            uint8 x = uint8(_cam_tmp10);
            _cam_tmp11 = x;
        } else if (!_cam_tmp9) {
            _cam_tmp11 = 0;
        }
        uint8 v = _cam_tmp11;
        return v;
    }

    function runParseU8Zero() external returns (uint8) {
        (bool _cam_tmp12, uint256 _cam_tmp13) = _cam_try_parse_radix("0", 10, 8);
        uint8 _cam_tmp14 = 0;
        if (_cam_tmp12) {
            uint8 x = uint8(_cam_tmp13);
            _cam_tmp14 = x;
        } else if (!_cam_tmp12) {
            _cam_tmp14 = 0;
        }
        uint8 v = _cam_tmp14;
        return v;
    }

    function runParseU16Max() external returns (uint16) {
        (bool _cam_tmp15, uint256 _cam_tmp16) = _cam_try_parse_radix("65535", 10, 16);
        uint16 _cam_tmp17 = 0;
        if (_cam_tmp15) {
            uint16 x = uint16(_cam_tmp16);
            _cam_tmp17 = x;
        } else if (!_cam_tmp15) {
            _cam_tmp17 = 0;
        }
        uint16 v = _cam_tmp17;
        return v;
    }

    function runParseU16Overflow() external returns (bool) {
        (bool _cam_tmp18, uint256 _cam_tmp19) = _cam_try_parse_radix("65536", 10, 16);
        bool _cam_tmp20 = false;
        if (_cam_tmp18) {
            _cam_tmp20 = false;
        } else if (!_cam_tmp18) {
            _cam_tmp20 = true;
        }
        bool bad = _cam_tmp20;
        return bad;
    }

    function runParseU32Max() external returns (uint32) {
        (bool _cam_tmp21, uint256 _cam_tmp22) = _cam_try_parse_radix("4294967295", 10, 32);
        uint32 _cam_tmp23 = 0;
        if (_cam_tmp21) {
            uint32 x = uint32(_cam_tmp22);
            _cam_tmp23 = x;
        } else if (!_cam_tmp21) {
            _cam_tmp23 = 0;
        }
        uint32 v = _cam_tmp23;
        return v;
    }

    function runParseU32Overflow() external returns (bool) {
        (bool _cam_tmp24, uint256 _cam_tmp25) = _cam_try_parse_radix("4294967296", 10, 32);
        bool _cam_tmp26 = false;
        if (_cam_tmp24) {
            _cam_tmp26 = false;
        } else if (!_cam_tmp24) {
            _cam_tmp26 = true;
        }
        bool bad = _cam_tmp26;
        return bad;
    }

    function runParseU128Max() external returns (uint128) {
        (bool _cam_tmp27, uint256 _cam_tmp28) = _cam_try_parse_radix("340282366920938463463374607431768211455", 10, 128);
        uint128 _cam_tmp29 = 0;
        if (_cam_tmp27) {
            uint128 x = uint128(_cam_tmp28);
            _cam_tmp29 = x;
        } else if (!_cam_tmp27) {
            _cam_tmp29 = 0;
        }
        uint128 v = _cam_tmp29;
        return v;
    }

    function runParseU128Overflow() external returns (bool) {
        (bool _cam_tmp30, uint256 _cam_tmp31) = _cam_try_parse_radix("340282366920938463463374607431768211456", 10, 128);
        bool _cam_tmp32 = false;
        if (_cam_tmp30) {
            _cam_tmp32 = false;
        } else if (!_cam_tmp30) {
            _cam_tmp32 = true;
        }
        bool bad = _cam_tmp32;
        return bad;
    }

    function runParseU256Max() external returns (uint256) {
        (bool _cam_tmp33, uint256 _cam_tmp34) = _cam_try_parse_radix("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", 16, 256);
        uint256 _cam_tmp35 = 0;
        if (_cam_tmp33) {
            uint256 x = _cam_tmp34;
            _cam_tmp35 = x;
        } else if (!_cam_tmp33) {
            _cam_tmp35 = 0;
        }
        uint256 v = _cam_tmp35;
        return v;
    }

    function runParseU256Overflow() external returns (bool) {
        (bool _cam_tmp36, uint256 _cam_tmp37) = _cam_try_parse_radix("1000000000000000000000000000000000000000000000000000000000000000", 16, 256);
        bool _cam_tmp38 = false;
        if (_cam_tmp36) {
            _cam_tmp38 = false;
        } else if (!_cam_tmp36) {
            _cam_tmp38 = true;
        }
        bool bad = _cam_tmp38;
        return bad;
    }

    function runParseI8Min() external returns (int8) {
        (bool _cam_tmp39, int256 _cam_tmp40) = _cam_try_parse_radix_signed("-128", 10, 8);
        int8 _cam_tmp41 = 0;
        if (_cam_tmp39) {
            int8 x = int8(_cam_tmp40);
            _cam_tmp41 = x;
        } else if (!_cam_tmp39) {
            _cam_tmp41 = 0;
        }
        int8 v = _cam_tmp41;
        return v;
    }

    function runParseI8Overflow() external returns (bool) {
        (bool _cam_tmp42, int256 _cam_tmp43) = _cam_try_parse_radix_signed("-129", 10, 8);
        bool _cam_tmp44 = false;
        if (_cam_tmp42) {
            _cam_tmp44 = false;
        } else if (!_cam_tmp42) {
            _cam_tmp44 = true;
        }
        bool bad = _cam_tmp44;
        return bad;
    }

    function runParseI16Min() external returns (int16) {
        (bool _cam_tmp45, int256 _cam_tmp46) = _cam_try_parse_radix_signed("-32768", 10, 16);
        int16 _cam_tmp47 = 0;
        if (_cam_tmp45) {
            int16 x = int16(_cam_tmp46);
            _cam_tmp47 = x;
        } else if (!_cam_tmp45) {
            _cam_tmp47 = 0;
        }
        int16 v = _cam_tmp47;
        return v;
    }

    function runParseI16Overflow() external returns (bool) {
        (bool _cam_tmp48, int256 _cam_tmp49) = _cam_try_parse_radix_signed("-32769", 10, 16);
        bool _cam_tmp50 = false;
        if (_cam_tmp48) {
            _cam_tmp50 = false;
        } else if (!_cam_tmp48) {
            _cam_tmp50 = true;
        }
        bool bad = _cam_tmp50;
        return bad;
    }

    function runParseI32Min() external returns (int32) {
        (bool _cam_tmp51, int256 _cam_tmp52) = _cam_try_parse_radix_signed("-2147483648", 10, 32);
        int32 _cam_tmp53 = 0;
        if (_cam_tmp51) {
            int32 x = int32(_cam_tmp52);
            _cam_tmp53 = x;
        } else if (!_cam_tmp51) {
            _cam_tmp53 = 0;
        }
        int32 v = _cam_tmp53;
        return v;
    }

    function runParseI32Overflow() external returns (bool) {
        (bool _cam_tmp54, int256 _cam_tmp55) = _cam_try_parse_radix_signed("-2147483649", 10, 32);
        bool _cam_tmp56 = false;
        if (_cam_tmp54) {
            _cam_tmp56 = false;
        } else if (!_cam_tmp54) {
            _cam_tmp56 = true;
        }
        bool bad = _cam_tmp56;
        return bad;
    }

    function runParseI64Min() external returns (int64) {
        (bool _cam_tmp57, int256 _cam_tmp58) = _cam_try_parse_radix_signed("-9223372036854775808", 10, 64);
        int64 _cam_tmp59 = 0;
        if (_cam_tmp57) {
            int64 x = int64(_cam_tmp58);
            _cam_tmp59 = x;
        } else if (!_cam_tmp57) {
            _cam_tmp59 = 0;
        }
        int64 v = _cam_tmp59;
        return v;
    }

    function runParseI64Overflow() external returns (bool) {
        (bool _cam_tmp60, int256 _cam_tmp61) = _cam_try_parse_radix_signed("-9223372036854775809", 10, 64);
        bool _cam_tmp62 = false;
        if (_cam_tmp60) {
            _cam_tmp62 = false;
        } else if (!_cam_tmp60) {
            _cam_tmp62 = true;
        }
        bool bad = _cam_tmp62;
        return bad;
    }

    function runParseI128Overflow() external returns (bool) {
        (bool _cam_tmp63, int256 _cam_tmp64) = _cam_try_parse_radix_signed("80000000000000000000000000000000", 16, 128);
        bool _cam_tmp65 = false;
        if (_cam_tmp63) {
            _cam_tmp65 = false;
        } else if (!_cam_tmp63) {
            _cam_tmp65 = true;
        }
        bool bad = _cam_tmp65;
        return bad;
    }

    function runParseRadix2() external returns (uint8) {
        (bool _cam_tmp66, uint256 _cam_tmp67) = _cam_try_parse_radix("11111111", 2, 8);
        uint8 _cam_tmp68 = 0;
        if (_cam_tmp66) {
            uint8 x = uint8(_cam_tmp67);
            _cam_tmp68 = x;
        } else if (!_cam_tmp66) {
            _cam_tmp68 = 0;
        }
        uint8 v = _cam_tmp68;
        return v;
    }

    function runParseRadix36() external returns (uint8) {
        (bool _cam_tmp69, uint256 _cam_tmp70) = _cam_try_parse_radix("z", 36, 8);
        uint8 _cam_tmp71 = 0;
        if (_cam_tmp69) {
            uint8 x = uint8(_cam_tmp70);
            _cam_tmp71 = x;
        } else if (!_cam_tmp69) {
            _cam_tmp71 = 0;
        }
        uint8 v = _cam_tmp71;
        return v;
    }

    function runParseBadRadix() external returns (bool) {
        (bool _cam_tmp72, uint256 _cam_tmp73) = _cam_try_parse_radix("10", 37, 64);
        bool _cam_tmp74 = false;
        if (_cam_tmp72) {
            _cam_tmp74 = false;
        } else if (!_cam_tmp72) {
            _cam_tmp74 = true;
        }
        bool bad = _cam_tmp74;
        return bad;
    }

    function runParseEmpty() external returns (bool) {
        (bool _cam_tmp75, uint256 _cam_tmp76) = _cam_try_parse_radix("", 10, 64);
        bool _cam_tmp77 = false;
        if (_cam_tmp75) {
            _cam_tmp77 = false;
        } else if (!_cam_tmp75) {
            _cam_tmp77 = true;
        }
        bool bad = _cam_tmp77;
        return bad;
    }

    function runParseInvalidDigit() external returns (bool) {
        (bool _cam_tmp78, uint256 _cam_tmp79) = _cam_try_parse_radix("12a3", 10, 64);
        bool _cam_tmp80 = false;
        if (_cam_tmp78) {
            _cam_tmp80 = false;
        } else if (!_cam_tmp78) {
            _cam_tmp80 = true;
        }
        bool bad = _cam_tmp80;
        return bad;
    }

    function runParse0xWrongRadix() external returns (bool) {
        (bool _cam_tmp81, uint256 _cam_tmp82) = _cam_try_parse_radix("0xff", 10, 64);
        bool _cam_tmp83 = false;
        if (_cam_tmp81) {
            _cam_tmp83 = false;
        } else if (!_cam_tmp81) {
            _cam_tmp83 = true;
        }
        bool bad = _cam_tmp83;
        return bad;
    }

    function runParseUintAlias() external returns (uint64) {
        (bool _cam_tmp84, uint256 _cam_tmp85) = _cam_try_parse_radix("99", 10, 64);
        uint64 _cam_tmp86 = 0;
        if (_cam_tmp84) {
            uint64 x = uint64(_cam_tmp85);
            _cam_tmp86 = x;
        } else if (!_cam_tmp84) {
            _cam_tmp86 = 0;
        }
        uint64 v = _cam_tmp86;
        return v;
    }

    function runParseIntAlias() external returns (int64) {
        (bool _cam_tmp87, int256 _cam_tmp88) = _cam_try_parse_radix_signed("-7", 10, 64);
        int64 _cam_tmp89 = 0;
        if (_cam_tmp87) {
            int64 x = int64(_cam_tmp88);
            _cam_tmp89 = x;
        } else if (!_cam_tmp87) {
            _cam_tmp89 = 0;
        }
        int64 v = _cam_tmp89;
        return v;
    }

    function runRoundTrip() external returns (uint64) {
        (bool _cam_tmp90, uint256 _cam_tmp91) = _cam_try_parse_radix(_cam_format_pad6(42, 6), 10, 64);
        uint64 _cam_tmp92 = 0;
        if (_cam_tmp90) {
            uint64 x = uint64(_cam_tmp91);
            _cam_tmp92 = x;
        } else if (!_cam_tmp90) {
            _cam_tmp92 = 0;
        }
        uint64 v = _cam_tmp92;
        return v;
    }

}

contract CambrianFactory {
    event Deployed(string entityType, address instance);

    address public owner;
    mapping(address => bool) public isDeployed;

    constructor() {
        owner = msg.sender;
    }

    function deployStdStrMatrix() external payable returns (address) {
        require(msg.sender == owner || isDeployed[msg.sender], "CambrianFactory: unauthorized");
        StdStrMatrix _instance = new StdStrMatrix{salt: bytes32(0), value: msg.value}(address(this));
        isDeployed[address(_instance)] = true;
        emit Deployed("StdStrMatrix", address(_instance));
        return address(_instance);
    }

    function predictStdStrMatrix() external view returns (address) {
        return address(uint160(uint256(keccak256(abi.encodePacked(
            bytes1(0xff), address(this), bytes32(0),
            keccak256(abi.encodePacked(type(StdStrMatrix).creationCode, abi.encode(address(this))))
        )))));
    }

}


---
// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;
// Auto-generated stub: re-exports StdStrMatrix from the combined project file.
import "./_std-str-matrix_project.sol";
