# Automated loop recovery

`fly-watchdog` remains report-only. The separate `fly-loop-recover.timer` checks its
`/run/fly/wd/loop.json` every five minutes. Two fresh suspected reports from
separate watchdog probes, at least ~5 minutes apart, cause **only** a
`flysim.service` restart. The ordinary checkpoint restore keeps the current
rung and learned brain state, but clears session macro ledgers. This does not
press buttons or promote a milestone archive. A successful restart imposes a
one-hour cooldown, including across intervening clear reports. The timer's
journal records every decision; inspect it with
`journalctl -u fly-loop-recover.service`. Disable the timer to stop automatic
recovery: `systemctl disable --now fly-loop-recover.timer`.

By default confirmation is deterministic, using watchdog's existing signal.
An optional OpenAI-compatible local router can veto a recovery: provision a
root-managed `/etc/fly/loop-recovery.env` readable by the `fly` account,
containing `FLY_LOOP_ROUTER_URL` (base URL ending in `/v1`) and
`FLY_LOOP_MODEL` (an available free-tier model). A private router can additionally
use `FLY_LOOP_ROUTER_KEY`; provision it outside this public checkout and limit
file permissions to `0640 root:fly`. Never put credentials in the unit or the
repository. When either router setting is present, both must be set; malformed or
unavailable responses prevent recovery. The model may only return `{"stuck": true|false}` and cannot
choose commands, buttons, or checkpoint paths. Confirm the model actually
exists and is reachable from the release container before configuring it.

This is an unstick mechanism, not a macro bug fix. A recurring trap still needs
the checkpoint-based loop review in `docs/loop-review.md`. The live recovery
must be recorded in the host claim log by the operator reviewing the unit
journal; the unit itself has no host claim-log access.

## Stream notice

So viewers are not left watching an unexplained freeze, the helper announces a recovery on the
stream through one file, `/run/fly/wd/recovery-notice.json` (`FLY_RECOVERY_NOTICE` overrides the
path for both writer and reader). It writes it atomically (a temporary file in the same directory,
then `rename`), readable by `fly`:

```json
{
  "v": 1,
  "id": "1790629095-reset",
  "phase": "countdown",
  "action": "reset",
  "fromRung": 12, "fromLabel": "MT. MOON",
  "toRung": 11, "toLabel": "PEWTER CITY",
  "reason": "unrewarded",
  "loop": ["GO OBJECTIVE", "GO WARP"],
  "stuckSeconds": 1800,
  "announcedAt": 1790629095,
  "executeAt": 1790629155,
  "updatedAt": 1790629160
}
```

`phase` runs `countdown` (about 60 s before `executeAt`) -> `acting` (flysim stopping,
restarting or being reset) -> `done` or `failed`; `id` is unique per recovery; `toRung`/`toLabel`
only for `action: "reset"`; every write refreshes `updatedAt`. `flystage-web` serves the file at
`/recovery-notice.json` and the page's recovery splash polls it once a second
(`apps/stage/README.md`, "Recovery splash"). The page ignores a notice more than 15 minutes old,
shows `done` for 8 s and `failed` for 20 s after their write, and stops covering the game 10 minutes
after an `acting` write that nothing followed, so the helper never needs to delete the file.
