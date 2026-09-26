# T-F-001 fuzz repro artifacts

When `audit_fuzz_parser_evm_forge_build` finds a validator-clean program that
still fails `forge build`, the harness writes a minimized repro here:

- `repro.cam` — generated source
- `project.yaml` — EVM project stub
- `NOTE.md` — forge/solc output excerpt

Subdirectories are named `tf001_<entity>_<route>_<ret|inc>/` or `tf002_route_seq/` (T-F-002 Foundry fuzz failure).
