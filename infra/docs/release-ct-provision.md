# Provisioning the release container — method

> **The record lives in the operator's infra repo** (`services/flybrain/release-ct-provision.md`), verbatim and dated:
> which host, which container id, which address and MAC, and what each step actually did.

Read `infra/README.md` for the host-side steps and the dev/release split, and
`infra/docs/runbook.md`'s "Cutting a release" for what happens after.

Method, once per container, from the operator's own env file passed by path:

1. `infra/provision.sh <release-env>` runs steps 01-07. On a first run there is no
   release artifact yet, so run it without `--release`: it converges every unit, config
   file and `bin/` helper and leaves the app units unstarted.
   `PRERELEASE_UNITS=1` is what lets step 05 converge units from an untagged tree.
2. Step 06 (secrets) reads `pass` on the **operator box**, not the host, so it refuses
   when run anywhere without the password store — that refusal is not the container's
   fault and does not invalidate steps 01-05.
3. Step 07 (enable) is deliberately last and deliberately manual: it is what turns the
   units on, and on a release container `flypush.service` stays disabled until the
   operator approves that run.
4. `infra/verify.sh <release-env>` is the gate. A second `provision.sh` run must
   converge nothing — `verify.sh`'s "no second-run restarts" check depends on the
   per-run log `provision.sh` pushes into the container.
5. The router-side reservation (DHCP plus a MAC reservation, not an in-guest static
   address) is requested by hand and confirmed free before provisioning. The address and
   the MAC are in the moved record, not here.

Claim the container in the host's agent claim log before any of this, and release it after
(`CLAUDE.md`).
