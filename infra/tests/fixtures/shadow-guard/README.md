# Recorded live-fly samples for `fly-shadow-run`'s guard

These samples come from a real `flysim` (legacy, FAFB, macros mode, at real time) on a shared build
box on 2026-09-30. They are in `fly-shadow-run`'s own format and cadence:

- `baseline.jsonl` holds 60 samples 10 s apart;
- `checks.jsonl` holds 20 samples a minute apart, after the same flysim restart that `start` does.

`lint.sh` runs `fly-shadow-run simulate` over each case and holds it to `expect`:

| Case | Setup | Expect |
| --- | --- | --- |
| `slow-alone` | 1 cpu, 1 thread; no shadow | pass |
| `slow-shadow-same` | the same fly, with a real `fly-shadow` following it at normal priority on the same cpu. The live fly dropped from 0.66 to 0.44-0.60 | trip |
| `fast-shadow-idle` | 3 cpus, 3 threads; a real `fly-shadow` at `SCHED_IDLE` on other cpus. The box was already loaded, and the fly ran at 0.4-0.85 | pass |
| `fast-alone` | 3 cpus, 3 threads; no shadow | pass |

The box's own load moved the fly between 0.4 and 1.0 during some baselines. Those steps are why
the baseline is robust: the median, and the spread taken as the MAD.
