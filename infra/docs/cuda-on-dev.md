# CUDA LIF backend on the dev container — method and verdict

> **The record lives in the operator's infra repo** (`services/flybrain/cuda-on-dev.md`), verbatim and dated.
> This file keeps only the method and the verdict.

**Verdict: PASS.** The CUDA LIF backend runs `flysim` end to end on the dev container —
the first time the *service*, rather than a benchmark binary, ran on the GPU. It holds real
time at well under one host core against the CPU kernel's two-and-a-bit on the same box,
the checkpoint moves across backends untouched, and nothing drifted over a three-hour
watch.

Method — five checks, in this order, none of them skippable:

1. **The feature is actually in the binary.** `FLY_CARGO_FEATURES=cuda
   infra/build/build-flysim.sh`. A binary without the feature ignores `FLY_LIF_CUDA=1`
   *silently*, so the check is flysim's own log line ("cuda backend attached"), never the
   env file.
2. **The container can see the card**: `GPU=1`, `/dev/nvidia*` present as character
   devices, userspace driver version in lockstep with the host module
   (`infra/verify.sh`'s GPU section).
3. **The PTX matches the card** — the committed PTX targets sm_75; another architecture
   needs `cuda/build-ptx.sh` re-run with `ARCH=`.
4. **Bit-exactness**, from `infra/docs/lif-cuda-spike.md`: the compatibility string and
   the kernel version string must not change, and a live checkpoint must restore across
   backends in both directions with no migration.
5. **Cost**, under `pidstat` over a fixed window: host cores held by the whole backend,
   compared with the CPU kernel on the same container at its own thread count.

Then `RAYON_THREADS` drops (the sweep and the propagation leave the CPU entirely; what
stays is the sim thread's serial work plus the sharded `plasticity.observe`), and
`cpuset_partition` hands the freed cpus to the page. **Turn `RAYON_THREADS` back up if
`FLY_LIF_CUDA` goes back to 0** — the reduced count is below real time for the CPU kernel.

Neighbouring guests are never touched by this work; the one that shares the card is read
only through the host's `nvidia-smi` process list.
