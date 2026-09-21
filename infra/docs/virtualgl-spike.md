# VirtualGL EGL spike — method

> **The record lives in the operator's infra repo** (`services/flybrain/virtualgl-spike.md`), verbatim and dated.
> This file keeps only the method and the verdict.

**Result: PASS.** `docs/design/gpu.md` section 3 option (d) works, and is now an infra
option: `CHROMIUM_PROFILE=vgl` (`infra/config/chromium-flags.vgl` plus
`vglrun -d egl0`). The kiosk gets hardware WebGL without leaving Xvfb: `__stage.fly()`
reports mode `webgl` instead of falling back to `paper`, the page keeps its 60 Hz rAF,
and total Chromium CPU drops by about a third of a core — clearing `gpu.md`'s 0.3-core
bar. The numbers are in the moved record.

Method, all of it read from the running kiosk, nothing simulated:

1. `__stage.fly()` mode, before and after, on the same page build.
2. Fly draw rate from `__stage.metrics().stages.fly.count` over a fixed window, and page
   rAF over the same window.
3. `getContext('webgl')` and the unmasked renderer string from inside the page;
   `chrome://gpu`'s webgl / 2d_canvas / gpu_compositing rows.
4. Chromium CPU under `pidstat` over a fixed window, **all** processes summed, split out
   by renderer — a per-process reading understates it.
5. x11grab, pulse and the encoder deliberately untouched, so the delta is the page's.

The .deb is an upstream artifact (Debian has no `virtualgl` package) and must be staged
on the host by hand with a mandatory sha256; upstream publishes no checksum file, only a
GPG signature inside the .deb's own `_gpgorigin` ar member, so verify that against the
upstream key before writing a hash into an env file.

**Why this stays off a release container:** `vgl` requires `--disable-gpu-sandbox`,
because VirtualGL's faker cannot open its second X11 connection inside a sandboxed GPU
process. The page renderer's sandbox is untouched, but the GPU process loses its namespace
sandbox while holding an ioctl handle on a kernel module shared with another guest. Fine on
a dev box; a deliberate decision for a 24/7 public stream.
