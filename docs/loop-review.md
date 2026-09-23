# Loop review: the standing authorization and the ethos check

The operator, 2026-09-17: "loop watcher. checks for loops. kicks off fable review agent." … "the loop
check auto improves the harness without oversight from me as long as changes match the ethos of
the project."

## The loop

1. The release watchdog (`infra/bin/fly-watchdog` check 10) flags a suspected loop: few distinct
   macros, a short sequence repeating, no growth in places explored. It exports
   `fly_loop_suspected` and writes `/run/fly/wd/loop.json`. It never acts.
2. The coordinator session (Fable) checks that marker on a schedule. On a flag it pulls the
   live checkpoint read-only (`pct pull`, into `.local/checkpoints/`, never committed), and
   spawns a review agent with the trap brief: reproduce from the checkpoint with the real
   cartridge and `examples/trap_hunt.rs`, name the mechanism, fix it inside the macros, prove
   it with a ROM-gated test and the trap hunt before/after, run the gates.
3. Fable reviews the diff against the ethos check below, merges with `--no-ff`, tags, builds
   from the tag, deploys with `05-deploy.sh` once, restarts flysim and flystage only, verifies
   on the encoder output and `/status`, and records the release in `docs/stream-mvp-plan.md`.
   No approval round trip with the operator; he sees the record.

## Unstick while a fix cooks (the operator, 2026-09-23)

"lets unstick the fly so stream stays interesting. lets make that a rule. while a fix cooks,
revert." When the watcher confirms a trap and its review agent is dispatched, the coordinator
restarts flysim only (claimed in the host log): a restore starts with empty macro ledgers and
keeps the rung. A recurrence gets another restart; only a fly still trapped about ten brain
minutes after a restart is rolled back with `fly-reset-to-milestone`. This is an action outside
the fly: it presses no button and changes no ROM, readout, catalog or adapter, and it never
replaces the fix, which still ships through the loop above.

## The ethos check (every one must hold, or the fix is not shipped)

- The ROM is never modified. Game memory is read, never written. The only write into the
  emulator is the joypad register. (`docs/design/macros.md` section 12.)
- Knowledge lives inside macros, never in the choice: no ranking, prior, weight, default
  action, or scripted objective decides which button the fly presses. A fix may change what a
  macro knows, what buttons the scene puts on the pad, and what a macro does once chosen.
- Nothing presses for the fly. No timeout, watchdog or fallback ever issues a button. A scene
  with nothing sensible to press has an empty pad and waits.
- The button readout is untouched: `lif-1ms-f64-v2`, `fly-kc-mbon-rstdp-v2`, the Game Boy
  preset's numbers, the direction and pulse channels. The macro group may only gain
  calibration or masking rules that apply equally to every channel.
- The reward catalog and the adapter version do not change. The compatibility string is
  byte-identical (`--print-compatibility`), so the live checkpoint carries over.
- Ledgers are session state, never checkpointed; a restore starts with no macro running.
- Every change carries a test that reproduces the trap and a ROM-gated proof where the
  cartridge is involved; `cargo test --workspace`, clippy, `infra/tests/lint.sh` green; the trap
  hunt from the triggering checkpoint improves (fewer flagged windows, more distinct tiles).
- Honesty on screen and in chat stays true: the mode chip, the pad, the ticker and the
  templates describe what actually runs.
- Commits are plain, no attribution trailers; only tagged commits reach the release box; the
  bridge and the push unit are not restarted for a macro fix.

Anything outside this list (a reward change, a decoder change, a new mode, a change to what
the fly is allowed to do) stops and waits for the operator.

## Scope (the operator: "scope. harness. macros. systems.")

The standing authorization covers three areas and nothing else:
- **harness**: the sim loop's macro layer, scene detection, ledgers, the trap hunt, benches,
  tests, the catalog's places and the map/geography data;
- **macros**: every macro script, palette binding and precondition;
- **systems**: infra, units, watchdog, deploy, capture, bridge plumbing, the stage's rendering
  of what already exists.
Out of scope without the operator: the brain (kernel, plasticity, readout numbers, populations), the
reward catalog and adapter version, new modes or doctrine, on-screen copy, anything that
changes what the fly is allowed to do.
