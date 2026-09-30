# Automated loop recovery

`fly-watchdog` remains report-only. The separate `fly-loop-recover.timer` reads its
`/run/fly/wd/loop.json` every minute and unsticks a confirmed trap on its own, climbing
a ladder one step per trap that outlives the previous step:

| level | step | cost |
|---|---|---|
| 0 | `systemctl restart flysim.service` | none: the restore keeps the rung and learned state, clears session macro ledgers |
| 1 | reset to the current rung's milestone archive | progress since the rung was first reached |
| 2+ | reset to the archive below the best rung (never lower) | one visible rung |

- **Confirmed** means two fresh suspected watchdog reports (at most 10 minutes old, `action: none`)
  from separate probes about 5 minutes apart. A clear report in between starts the count again.
- **Outlives** means the reports that confirm it again are at least 20 minutes after the step, so
  their 10-brain-minute window lies after it. The helper waits that long after every step.
- **Budget:** at most two milestone resets per 24 hours. With the budget spent the step is a
  restart, at most every three hours, until a reset is free again. The same three-hour spacing applies
  when a reset level finds no restorable rung and restarts instead, and between any two restarts in a
  row, also when the ladder has started over in between.
- **Starting over:** the ladder returns to level 0 when the fly reaches a new best rung, after
  six hours with no suspected report, or when the fly makes new lasting progress more than
  20 minutes after the last step and a clear probe shows the old trap let go (row 70). Lasting
  progress is the watchdog's `window.progress`: a rung reached, a species owned, a map entered, or
  a trainer or badge beaten, each named in `window.progress.keys` as `kind:label` (`pokedex:OWNED
  #41`). A ring cannot earn them twice, but a milestone reset restores the reward ledger, so its
  replay pays the archive's rewards again. Progress is therefore measured against every name the
  ladder has seen (`progressSeen` in the state file, the last 2,000), not against the restored
  archive: a replay re-earning what the run had before the reset is not progress, and the ladder
  climbs on to the rung below as it did before row 70. A trap that follows new progress is a new
  trap, and it starts again from a restart. A catch made while the fly stays flagged is not a new
  trap. With a watchdog that reports no names, the counts are used as before.
  Names the ladder never saw (before this version was deployed, or while an older one ran) count
  as new once.
- **Protected catches:** a milestone reset never erases a species first owned in the last two hours
  (`PROTECT`, a `pokedex:` name not seen before). A species a reset's replay owns again was the fly's
  before the reset and was protected then, so it does not open a second window. Within that window the step is a restart, spaced by
  the hold like a spent budget, and `history.jsonl` records a `protect` event. A reset turned into a
  restart this way does not climb the ladder: the level keeps its slot and the reset comes, at the
  same rung, once the window ends. Evolution also counts as a species owned.
- **No archive twice in a row:** a reset never restores the same archive as the last reset,
  until a new best rung is reached or twelve hours have passed (`AGAIN`; a trap no restart clears
  is reset again in bounded time, no later than before this rule). Such a step does climb, so the rung below is reached promptly. A restore is deterministic, so the same archive replays the
  same run into the same trap. Row 70's second reset to rung 11 matched the first one event for
  event: the same Zubat caught, the same trap an hour later, and the catch erased. That step is
  a restart instead. A reset that fails does not mark its archive: it is not known to have been
  restored, so the next reset may try it again.

A milestone step only uses an archive the running build can restore. `fly-loop-reset --list`
compares each archive's compatibility string with `flysim --print-compatibility` and accepts an
adapter-only difference that `FLY_ACCEPT_ADAPTERS` in `/etc/fly/fly.env` names (the rule
`05-deploy.sh` applies). A flysim that refuses every checkpoint refuses to start, so a step whose
rung is not restorable is a restart instead, never a lower rung. Keep `FLY_ACCEPT_ADAPTERS` in the
release env file so a deploy does not drop it.

The step runs `/opt/fly/bin/fly-loop-reset <rung>` through sudo (the one line in
`config/fly-sudoers`). As root it:

- runs only the root-owned `/opt/fly/sbin/flysim`, and only while it is byte-identical to the
  flysim `/opt/fly/current` points to, that `05-deploy.sh` installs from the release
  tarball after checking it against the tarball's MANIFEST (never the fly-owned release tree).
  Without that copy (an infra-only deploy, or a manual rollback) no rung is restorable and the
  ladder only restarts; after a release deploy, check as `fly` that
  `sudo -n /opt/fly/bin/fly-loop-reset --list` names the current rung;
- reads `fly.env` as `KEY=VALUE` data, never sources it, and ignores the caller's environment;
- stops `fly-watchdog.timer` and waits for a running probe to finish, so the watchdog cannot
  start flysim on a half-rewritten store; stops flysim; runs `fly-reset-to-milestone`; and on every
  exit path starts flysim and the watchdog timer again.

The reset copies both stores to `/srv/fly/state.reset-<UTC>` first, as in the runbook, and
clears milestone archives above the rung. After each step the helper waits up to four minutes for
`/status` to report `running` (and the target rank for a reset). Otherwise the step is recorded
as failed and the ladder still climbs. The step is recorded before it runs, so a helper killed
mid-step has still climbed and spent the reset. Do not stop `fly-loop-recover.service` during a
step: that kills the reset too, and the wrapper's exit trap starts flysim on whatever state is
left.

## On stream

Every step is announced 60 seconds ahead (`FLY_LOOP_COUNTDOWN`) in
`/run/fly/wd/recovery-notice.json` (`FLY_RECOVERY_NOTICE`), which the stage's recovery splash
reads. It holds the contract below, written atomically, mode 0644:

```json
{"v": 1, "id": "1790629095-reset", "phase": "countdown|acting|done|failed",
 "action": "restart|reset", "fromRung": 12, "fromLabel": "MT. MOON",
 "toRung": 11, "toLabel": "BOULDER BADGE", "reason": "unrewarded",
 "loop": ["GO OBJECTIVE", "GO WARP"], "stuckSeconds": 900,
 "announcedAt": 1790629095, "executeAt": 1790629155, "updatedAt": 1790629160}
```

`toRung`/`toLabel` are present for a reset only; every write refreshes `updatedAt`, and the
helper never deletes the file. `flystage-web` serves it at `/recovery-notice.json` (204 when
absent) and the page's recovery splash polls it once a second (`apps/stage/README.md`, "Recovery
splash"). The page ignores a notice more than 15 minutes old, shows `done` for 8 s and `failed`
for 20 s, and stops covering the game 10 minutes after an `acting` write that nothing followed.
A milestone step pauses the watchdog, so Chromium is not restarted under the splash; a plain
restart is short enough that a watchdog pass rarely lands in it.

## Model confirmation

An OpenAI-compatible router can be asked before each step. The model may only answer
`{"stuck": true|false}` (the reply may wrap it in prose; the first such object counts). It cannot
choose commands, rungs or buttons. `false` delays the step by one probe, at most three times in a
row; then the step goes ahead. If no model answers (unset, rate-limited, down, malformed), the
watchdog's confirmation stands alone. A model can delay recovery by about 15 minutes, never deny
it.

Provision a root-managed `/etc/fly/loop-recovery.env`, mode `0640 root:fly`, outside this public
checkout:

```
FLY_LOOP_ROUTER_URL=<base URL ending in /v1>
FLY_LOOP_ROUTER_KEY=<router key>
FLY_LOOP_MODELS=<model>,<fallback model>,...
```

Models are tried in order within a 90-second budget. Prefer fast free-tier chat models, and put
providers with generous free limits first: free OpenRouter models share a small daily quota and
the router's circuit breaker can close the whole provider for a while. Check each model against
a stuck and a healthy report before listing it. `FLY_LOOP_MODEL` (one model) is still read when
`FLY_LOOP_MODELS` is unset.

## Operating it

- Decisions: `journalctl -u fly-loop-recover.service`. Every step, veto and ladder restart is also
  appended to `/var/lib/fly-loop-recover/history.jsonl`.
- Ladder state: `/var/lib/fly-loop-recover/state.json` (the unit's `StateDirectory`, so it
  survives a reboot). Deleting it starts the ladder over.
- Stop automatic recovery: `systemctl disable --now fly-loop-recover.timer`.
- Record automatic steps you find in the journal in the host claim log when you next claim the
  container; the unit has no access to that log.

This is an unstick mechanism, not a macro bug fix. A recurring trap still needs the
checkpoint-based loop review in `docs/loop-review.md`.
