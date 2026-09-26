#!/usr/bin/env bash
# Copyright (C) 2025-2026 The Cambrian Authors
# SPDX-License-Identifier: GPL-3.0-only

# Fail if this repo contains git submodules / gitlinks.
# forge-std must be a plain checkout (`forge install --no-git`), never a
# mode-160000 gitlink. See docs/plans/forge-std-no-gitlink.md.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

if [ -e .gitmodules ]; then
  echo "error: .gitmodules must not exist (forge-std was being registered as a submodule)" >&2
  if [ -f .gitmodules ]; then
    cat .gitmodules >&2
  fi
  exit 1
fi

links="$(git ls-files -s | awk '$1=="160000"')"
if [ -n "$links" ]; then
  echo "error: gitlinks (mode 160000) are forbidden in this repo:" >&2
  echo "$links" >&2
  exit 1
fi
