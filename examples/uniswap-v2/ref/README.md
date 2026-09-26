# Reference contracts

The Cambrian re-implementation in `examples/uniswap-v2/` mirrors the
**behavioural spec** of Uniswap V2 Core, pinned to commit
[`6a9e7c97860676e0992f22a49665760444c1cdf5`](https://github.com/Uniswap/v2-core/tree/6a9e7c97860676e0992f22a49665760444c1cdf5)
on `Uniswap/v2-core`.

This directory keeps the reference Solidity verbatim so it is trivial
to diff, audit, and re-run alongside the generated Cambrian output.

## Layout

```
ref/
  core/
    UniswapV2Pair.sol          (v2-core contracts/UniswapV2Pair.sol)
    UniswapV2Factory.sol       (v2-core contracts/UniswapV2Factory.sol)
    UniswapV2ERC20.sol         (v2-core contracts/UniswapV2ERC20.sol)
  target/
    FlashCallback.sol          (trivial flash-swap callee; not from Uniswap)
  LICENSE                      (GPL-3.0 — Uniswap V2 Core)
```

## Provenance

```
git clone https://github.com/Uniswap/v2-core.git
git checkout 6a9e7c97860676e0992f22a49665760444c1cdf5
```

The vendored files under `core/` are unmodified.

## Note

These are the **reference** contracts only. The Cambrian translations
live one directory up (`ERC20.cam`, `UniswapV2Pair.cam`,
`UniswapV2Factory.cam`) and are not line-for-line ports — they
re-express the same business logic in idiomatic Cambrian (member-centric
state, deterministic CREATE2 addresses with **two `identity` members**
on `Pair`, `from <member>` sender checks, phased routes with
cross-contract `var` capture, `.fold(...)` for Babylonian sqrt, and
`hashOf(...)` for pair keys).

The mapping between Uniswap V2 patterns and the Cambrian idioms picked
here is documented in `examples/uniswap-v2/PLAN.md`.

## Licence note

`UniswapV2Pair.sol`, `UniswapV2Factory.sol`, and `UniswapV2ERC20.sol`
are licensed under **GPL-3.0**, matching the upstream
`Uniswap/v2-core` repository (`LICENSE` is preserved here unmodified).
The Cambrian translations in the parent directory are *new
expressions* of the same algorithm, written from the spec, and carry
the licence of the host workspace; they share no source text with the
GPL-3.0 reference files. `target/FlashCallback.sol` is original and
licensed MIT.
