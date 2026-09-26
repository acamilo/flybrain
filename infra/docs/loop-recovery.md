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
repository. When a router is configured, malformed or unavailable responses
prevent recovery; the model may only return `{"stuck": true|false}` and cannot
choose commands, buttons, or checkpoint paths. Confirm the model actually
exists and is reachable from the release container before configuring it.

This is an unstick mechanism, not a macro bug fix. A recurring trap still needs
the checkpoint-based loop review in `docs/loop-review.md`. The live recovery
must be recorded in the host claim log by the operator reviewing the unit
journal; the unit itself has no host claim-log access.
