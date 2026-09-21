# Limitations

Everything here is carried over from the prototype's own documentation
(`fly-plays-pokemon/docs/architecture.md`, `docs/rewards-learning.md`, `README.md`). None of it
has been superseded by the extraction into this library.

## No claim of biological fidelity

The connectivity is real: it comes from the FlyWire FAFB Codex v783 export. Almost nothing else
is.

- Role names are anatomical labels from the Codex annotations, not functions inferred from
  behaviour. The `command_0..7` buckets are a round-robin partition of the descending population
  by neuron index, and the transmitter sign table is a modeling choice
  (`tools/README.md`).
- The LIF kernel is a generic point-neuron model: one membrane variable per neuron, a 20-ms decay,
  a fixed threshold, a 2-tick refractory period and a scalar weight multiplier. There are no
  compartments, no channel dynamics, no synaptic delays and no neuromodulator diffusion.
- The plastic sites are anatomical, not fitted dopamine compartments, and the modulator is a
  synthetic scalar supplied by the application. No claim is made that this rule is a
  quantitatively fitted fly learning mechanism.
- PAM stimulation and the learning signal are separate code paths. PAM spiking does not causally
  supply the plasticity modulator.

## The retina is not fly optics

One L1 column reads exactly one pixel, chosen by normalizing the column's 2D coordinate into the
frame and rounding to the nearest pixel. There is no lens model, no ommatidial sampling geometry,
no temporal filtering and no motion pathway. The left hemisphere is handled by mirroring the X
axis. The original doc says it plainly: no claim is made that the retinal mapping resembles fly
optics.

## Learning has not been shown to improve play

Plasticity works as specified: eligibility accumulates on causal pairs, reinforcement moves gains,
and the tests cover timing, bounds, scope and zero-reward invariance. What is not shown is that
any of it helps.

- The prototype's browser integration was a short boot and recovery test. It did not autonomously
  complete a trainer fight, a capture, a badge or the game.
- It reported zero changed gains during boot, which is expected without gameplay rewards.
- Long-run task learning remains unproven, as does whether the reward weights improve Pokemon
  performance.
- Battle semantic tests used synthetic WRAM traces grounded in disassembly, not observed play.
- The reward detectors are conservative and can miss a transition and under-reward. The wild-KO
  detector is not proven across every battle edge case, such as simultaneous faints and unusual
  scripted battles.

Tests establish mechanism and recovery, not task competence or long-run biological validity.

## Throughput is below real time

The prototype measured roughly 20.9 emulator fps in a ROM-backed browser run, after an
optimization that replaced eligibility observation's repeated base-network traversal with
precomputed sparse slot lists. Earlier snapshots were 16.6 to 17.5 fps. All of these are short-run
diagnostics on one host under Playwright, not controlled or sustained benchmarks.

A Game Boy runs at 59.7275 fps, so about 20 fps is roughly a third of real time. The scheduler
waits between frames and does not guarantee real-time speed. `docs/streaming-plan.md` notes the
consequence for a second demo: a platformer punishes latency far harder than Pokemon does, and at
about 20 effective fps precise jumps may be impossible.

Stable throughput remains unproven.

## Checkpoint compatibility is strict

A checkpoint is only loadable against the same kernel version, the same dataset fingerprint and
the same plasticity version and topology hash, and every component validates before it mutates.
This is deliberate: mismatched experiments do not silently migrate.

The consequences to plan for:

- Changing any numeric kernel or rule parameter changes a version string and invalidates every
  existing checkpoint for that configuration.
- Changing semantics without changing a number requires bumping `NEURAL_KERNEL_VERSION` or
  `PLASTICITY_VERSION` by hand. Nothing detects a forgotten bump.
- Rebuilding the dataset with different options, or from a different Codex version, changes the
  fingerprint and invalidates checkpoints even when the kernel is unchanged.
- The prototype's schema-1 migration was explicitly best-effort and lossy: it discarded old
  plastic gains and eligibility, widened Float32 spike timestamps, and rebaselined reward
  semantics. Old checkpoints had no data fingerprint and could not establish exact compatibility.
  A non-finite old neural state was rejected outright, with no automatic clean-start overwrite.

The library provides the version strings and the validation. Envelope format, CRC, atomic writes
and rollback are the application's responsibility; the prototype's contract is summarized in
[integration](integration.md).

## Long-run behaviour is unverified

The tests run for milliseconds to seconds of simulated time. The longest oracle comparison is
3,000 ms on the toy dataset and 200 ms on the real dataset.

Unverified over hours or days: whether gains settle, saturate at the `[0.9, 1.1]` clamps or
oscillate; whether the restoring term is the right size; whether Float64 eligibility timestamps
and Float32 gains stay well conditioned; whether the 30-second Start/Select cooldown behaves
sensibly across long idle stretches; and whether throughput holds.

One of these is no longer unverified, and the answer was not the reassuring one. Forty-three brain
minutes of the v0.1.0 release run were measured from its own checkpoint
(`infra/docs/room-escape.md`): the readout's habituation was working exactly as specified, but
because `calibrate()` runs once at warm-up, the four direction scores had drifted into a fixed
preference order that no amount of activity could reorder, and habituation was the only thing left
that could move the winner at all. That is a long-run property of a readout calibrated once, and it
was invisible to every test shorter than half an hour and to the random walker, which has no
incumbency to freeze.

The `clearEligibility()` and `clearHolds()` escape hatches exist but no policy for using them has
been validated.

Getting stuck is a valid outcome for a demo built on this library.
