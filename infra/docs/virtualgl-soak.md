# VirtualGL 3 h soak — method

> **The record lives in the operator's infra repo** (`services/flybrain/virtualgl-soak.md`), verbatim and dated.
> This file keeps only the method.

The VirtualGL spike owed a long observation before `CHROMIUM_PROFILE=vgl` could go
anywhere near a release container: the longest continuous look at that point was about
25 minutes. This is that soak — **passive and read-only**. Nothing was installed,
configured or restarted for it.

Every 10 minutes for 3 hours, on the dev container running the `vgl` kiosk:

- `pidstat` over a 30 s window, summed across all `chromium` processes inside the
  container;
- `nvidia-smi --query-compute-apps` and `--query-gpu` on the host;
- `__stage.fly()` mode and draw fps (from the `stages.fly.count` delta over the same
  window as the rAF sample), `__stage.health()` gaps / decodeErrors / accepted, and rAF Hz;
- the HLS output re-probed hourly from the operator box against the container's own
  MediaMTX endpoint.

What the soak is looking for is drift, not a peak: a mode that silently falls back to
`paper`, a creeping renderer CPU figure, VRAM growth, or a fps figure that decays while
rAF stays flat.
