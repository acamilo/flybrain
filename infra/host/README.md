# infra/host — the two files that run on the host itself

Everything else under `infra/` runs against a container. These two run on the **host**,
because they write `/etc/pve/lxc/<id>.conf`, which no `pct exec` can reach, and because
they have to be ordered against `pve-guests.service`, which is not a per-container
concern.

They are installed **by hand**, by the operator, once. `provision.sh` does not install them and
must not: a boot-ordered host unit that rewrites a production container's config
(the neighbouring GPU container) is not something a per-demo provisioning script should be doing behind the
operator's back.

| File | Installs as |
|---|---|
| `fly-nvidia-majors.sh` | `/usr/local/sbin/fly-nvidia-majors.sh`, mode `0755`, root:root |
| `fly-nvidia-majors.service` | `/etc/systemd/system/fly-nvidia-majors.service`, mode `0644` |

Background: `docs/design/gpu.md` section 1 ("The dynamic `nvidia-uvm` major").
`/dev/nvidia-uvm`'s and `/dev/nvidia-caps`'s majors are allocated dynamically and change
across boots (uvm moved 509 → 511 on the LLM host on 2026-08-29), so a literal major written
once into a container config goes stale silently. `195` (nvidia0 / nvidiactl /
nvidia-modeset) is compile-time static and never drifts.

## Install

From a checkout of this repo on the host, as root:

```sh
install -o root -g root -m 0755 infra/host/fly-nvidia-majors.sh /usr/local/sbin/fly-nvidia-majors.sh
install -o root -g root -m 0644 infra/host/fly-nvidia-majors.service /etc/systemd/system/fly-nvidia-majors.service

# Look before you leap: --dry-run prints the diff it would apply and writes nothing.
/usr/local/sbin/fly-nvidia-majors.sh --dry-run 122 150 151 199

systemctl daemon-reload
systemctl enable --now fly-nvidia-majors.service
systemctl status fly-nvidia-majors.service     # want: active (exited), Main PID ... (code=exited, status=0/SUCCESS)
```

Expected first-run output on a host that has never had this: `122 changed` (its stale
`c 509:*` / `c 234:*` lines get corrected), and `missing` for any fly CT that does not
exist yet. A second run must print `unchanged` for every existing CT — if it does not,
something else is rewriting those configs and that needs explaining before going further.

**A `pct stop`/`pct start` will shuffle the file, and that is normal.** PVE re-emits the
conf on every lifecycle operation with its own keys sorted, every comment hoisted to the top
and every raw `lxc.*` key moved to the end, so after one container restart the
`# BEGIN fly-nvidia` / `# END fly-nvidia` pair sits at the top of the file with *nothing*
between it and the lines it generated sit at the bottom. That is expected. The script
converges lines rather than bytes and still reports `unchanged` (verified on the host,
2026-09-16, and asserted in `infra/tests/lint.sh`); if it ever reports `changed` on a second
consecutive run, that is the bug to chase.

Installed on the host on 2026-09-16 (GPU run for the dev container). First run: `122 changed`, `199
changed`, `150`/`151 missing`. The neighbouring GPU container's own `c 509:* rwm` line is **left alone on purpose**:
`509` is `mei` (the Intel ME interface) in this host's `/proc/devices` today, not
`nvidia-uvm`, and the script never deletes an `allow` line for a major that resolves to a
non-NVIDIA device. It is reported on every run and wants a by-hand deletion from whoever
owns the neighbouring GPU container.

**the neighbouring GPU container is in the default id list on purpose, and the operator approved it (2026-09-15).** It is
a production container running the GPU workload in the neighbouring container on this same card, its majors are currently
stale (`docs/design/gpu.md` section 0), and including it repairs them. It is also the
reason this file exists as a hand-install step rather than a scripted one. Do not extend
the list to other people's containers without the same conversation.

`pct stop` / `pct start` is needed for a config change to take effect on a **running**
container — `lxc.*` keys are read only at container start. At boot that is automatic
(this unit is `Before=pve-guests.service`). Mid-life, restart the container yourself;
`infra/01-create-ct.sh` does that for the fly CTs when it sees the block change.

## Reboot order

`docs/runbook.md`'s "the host reboot order" section carries the operational version of
this. The short form:

```
nvidia-devnodes.service   (materialises /dev/nvidia*)
    ->  fly-nvidia-majors.service   (rewrites the majors into the guest configs)
        ->  pve-guests.service      (starts the containers, which read those configs)
```

Both NVIDIA units must complete before `pve-guests.service`. The fly containers'
`startup order=4,up=60` is what keeps them behind it. After any reboot of the host, the
check that matters is `infra/verify.sh <release-env>`, whose GPU section asserts
that the majors in the conf still match `/proc/devices` and that every `/dev/nvidia*`
inside the container is a **character device** rather than the empty regular file that
`bind,optional,create=file` leaves behind when the source node was missing at start.

## Optional host hygiene, not installed here

- `nvidia-smi -pm 1` (persistence mode). With a 24/7 client attached it changes little,
  and it is a host-wide change, so it stays a deliberate the operator decision rather than a
  line in a script. `docs/design/gpu.md` section 5.
- **Do not** set `nvidia-smi -c EXCLUSIVE_PROCESS`. Compute mode stays `Default`;
  exclusive mode would let whichever container got there first lock the card and would
  break the neighbouring GPU container.
