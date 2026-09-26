# Audit hypothesis tests (EVM + Lean)

Fixtures that turn static-review hypotheses into executable tests.
Foundry behaviour is verified with `forge test` / `forge build`, never
raw `solc`.

```
audit/
  fixtures/           # minimal .cam + project.yaml per hypothesis
  forge/              # hand-written Forge test wrappers (EVM)
  README.md           # this file
```

Language surface: [docs/LANGUAGE.md](../../../docs/LANGUAGE.md).
Test policy: [docs/TESTING_TARGETS.md](../../../docs/TESTING_TARGETS.md).
