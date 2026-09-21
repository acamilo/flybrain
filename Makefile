# The same gates CI runs, in the same order, on this box.
#
#   make ci     install -> artifacts -> test -> typecheck -> rust -> lint
#   make e2e    the Playwright suite (allowed to fail in CI; run it by hand here)
#   make all    ci + e2e
#
# `npm run ci` is the same chain minus `npm ci` itself (an npm script cannot safely wipe the
# node_modules it is running out of), so `make install && npm run ci` == `make ci`.
#
# Nothing here needs a ROM, a GPU or a secret. FLY_ROM stays unset, so the ROM-gated Rust tests
# skip with a note on stderr; set it yourself for a full local run:
#   FLY_ROM="$$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" make rust

SHELL := /usr/bin/env bash
.SHELLFLAGS := -euo pipefail -c
.DEFAULT_GOAL := ci

.PHONY: ci all install artifacts test typecheck rust lint e2e browsers clean

ci: install artifacts test typecheck rust lint

all: ci e2e

install:
	npm ci

# The FlyWire connectome is committed (11 MB, CC BY-NC 4.0 — data/fafb-v783/ATTRIBUTION.md), so
# this verifies rather than downloads. Only the raw multi-GB Codex exports are fetched, and only
# by `uv run python3 tools/build_flywire.py`, which CI never runs.
artifacts:
	cd data/fafb-v783 && sha256sum -c ../../tools/artifact-checksums.txt

test:
	npm test

typecheck:
	npm run typecheck

# --release, not debug: the flysim integration test that asserts the feed's 30 Hz contract only
# reaches ~9.5-10.4 Hz in a debug build (known, pre-existing — infra/docs/macros-traps.md). For a
# debug run, skip exactly that test:
#   cd services/flysim && cargo test --workspace -- \
#     --skip the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed
rust:
	cd services/flysim && cargo test --workspace --release

lint:
	infra/tests/lint.sh

browsers:
	cd apps/stage && npx playwright install --with-deps chromium

# Screenshot comparisons at maxDiffPixelRatio 0.002 against the real `vite build` output. Font
# rasterisation and timing make this flaky on a shared runner, which is why CI marks the job
# allowed-to-fail; locally it is a real gate.
e2e:
	npm run test:e2e --workspace @flybrain/stage

clean:
	rm -rf apps/stage/playwright-report apps/stage/test-results services/flysim/target
