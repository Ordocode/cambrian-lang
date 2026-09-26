# Reference contracts

The Cambrian re-implementation in `examples/governor/` mirrors the
**behavioural spec** of OpenZeppelin's governance suite, pinned to release
**`v5.0.2`**.

This directory keeps the reference Solidity verbatim so that it is trivial
to diff, audit, and re-run alongside the generated Cambrian output.

## Layout

```
ref/
  governor/
    Governor.sol              (OZ v5.0.2 contracts/governance/Governor.sol)
    TimelockController.sol    (OZ v5.0.2 contracts/governance/TimelockController.sol)
  token/
    ERC20Votes.sol            (OZ v5.0.2 contracts/token/ERC20/extensions/ERC20Votes.sol)
  target/
    CounterTarget.sol         (trivial sink contract; not from OZ)
```

## Provenance

```
git clone https://github.com/OpenZeppelin/openzeppelin-contracts.git
git checkout v5.0.2
```

The vendored files are unmodified.

## Note

These are the **reference** contracts only. The Cambrian translations live
one directory up (`ERC20Votes.cam`, `TimelockController.cam`,
`Governor.cam`) and are not line-for-line ports — they re-express the same
business logic in idiomatic Cambrian (member-centric state, deterministic
CREATE2 addresses, `from <Entity>(...)` sender checks, `enum`+`match`
state machines, `hashOf(...)`, EVM `var` capture across phases).

The mapping between OZ patterns and the Cambrian idioms picked here is
documented in `examples/governor/PLAN.md`.
