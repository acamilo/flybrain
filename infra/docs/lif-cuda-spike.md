# LIF-on-CUDA spike — method

> **The record lives in the operator's infra repo** (`services/flybrain/lif-cuda-spike.md`), verbatim and dated.
> This file keeps only the method and what it settled.

**Bit-exactness: PASS.** 10,000 ticks on `data/fafb-v783`, comparing membrane,
refractory, the spike list, the RNG, the clock, the rates and the whole plasticity state
after **every** tick: zero divergences. The existing golden suite also passes through the
GPU backend unchanged. `docs/design/gpu.md` section 6 said not to try this; section 6 is
wrong, and the moved record says why.

**Speed: not a win in absolute terms, a large win per CPU core.** The GPU tick reaches
roughly the same realtime factor as the CPU kernel at four whole Haswell cores, on **one**
host core — and hands the other three to the page and the encoder, which is the resource
the infra docs keep fighting over. That is the whole argument for the backend; the
per-box figures are in the moved record.

Method: run the same seeded configuration through both backends, comparing state after
every tick rather than at the end (a late divergence is invisible in an end-state diff);
then measure the tick in isolation and the backend's host-core cost under `pidstat`; then
restore a checkpoint taken on each backend onto the other.
