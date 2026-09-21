/**
 * The SFX bank, synthesised rather than shipped (design A8 asks for six short samples).
 *
 * Eight procedurally generated sounds, rendered once into `AudioBuffer`s with an
 * `OfflineAudioContext` and oscillators, so the repo carries no audio assets and no third-party
 * licence: a reward tick, a badge fanfare, a milestone chime, a sugar sparkle, the stuck-threshold
 * tone, a feed-stale alarm, the rollback's rewind sweep and the day-rollover stinger. The last two
 * are the gap `src/motion/sfx-tiers.ts` recorded as `NEEDS_SAMPLE`: its `rewind` tier was playing
 * the stuck alarm and its `stinger` the milestone chime at half level, so both moments sounded
 * like something they were not. They are deliberately plain shapes at modest level — this plays
 * *under* game audio on a 24/7 stream, so the design goal is "noticeable once", not "musical".
 *
 * Every entry point is guarded: a suspended or unavailable AudioContext must never throw, because
 * an exception here would take the broadcast page down for a sound effect.
 */

export type SfxName = 'reward' | 'badge' | 'milestone' | 'sugar' | 'stuck' | 'stale' | 'rewind' | 'stinger';

export const SFX_NAMES: readonly SfxName[] = [
  'reward',
  'badge',
  'milestone',
  'sugar',
  'stuck',
  'stale',
  'rewind',
  'stinger',
];

interface Voice {
  /** Oscillator type. */
  type: OscillatorType;
  /** Frequency envelope as [timeFraction, Hz] pairs. */
  freq: readonly [number, number][];
  /** Gain envelope as [timeFraction, gain] pairs. */
  gain: readonly [number, number][];
  /** Start offset as a fraction of the sample duration. */
  start?: number;
}

interface Recipe {
  durationSeconds: number;
  voices: readonly Voice[];
}

/**
 * The recipes. Kept declarative so a sound can be retuned without touching the renderer.
 */
const RECIPES: Record<SfxName, Recipe> = {
  // A soft blip: one short sine, the sound of a +0.05 exploration tick.
  reward: {
    durationSeconds: 0.09,
    voices: [
      {
        type: 'sine',
        freq: [
          [0, 880],
          [1, 1180],
        ],
        gain: [
          [0, 0],
          [0.1, 0.5],
          [1, 0],
        ],
      },
    ],
  },
  // Badge: a rising triad, the loudest thing in the bank.
  badge: {
    durationSeconds: 0.75,
    voices: [
      { type: 'triangle', freq: [[0, 523]], gain: [[0, 0], [0.05, 0.5], [0.45, 0.3], [1, 0]] },
      { type: 'triangle', freq: [[0, 659]], gain: [[0, 0], [0.05, 0.4], [0.6, 0.25], [1, 0]], start: 0.12 },
      { type: 'triangle', freq: [[0, 784]], gain: [[0, 0], [0.05, 0.4], [0.7, 0.25], [1, 0]], start: 0.26 },
      { type: 'sine', freq: [[0, 1046]], gain: [[0, 0], [0.1, 0.25], [1, 0]], start: 0.4 },
    ],
  },
  // Milestone: two notes, a step up. Quieter than a badge; it happens more often.
  milestone: {
    durationSeconds: 0.45,
    voices: [
      { type: 'sine', freq: [[0, 587]], gain: [[0, 0], [0.08, 0.4], [1, 0]] },
      { type: 'sine', freq: [[0, 880]], gain: [[0, 0], [0.08, 0.35], [1, 0]], start: 0.2 },
    ],
  },
  // Sugar: a quick sparkle up, to go with the dopamine bar jumping.
  sugar: {
    durationSeconds: 0.3,
    voices: [
      {
        type: 'sine',
        freq: [
          [0, 660],
          [1, 1760],
        ],
        gain: [
          [0, 0],
          [0.06, 0.4],
          [1, 0],
        ],
      },
      { type: 'square', freq: [[0, 220]], gain: [[0, 0], [0.05, 0.08], [0.4, 0]] },
    ],
  },
  // Stuck threshold: a low, flat, slightly ominous pair. This is the "interesting part" cue.
  stuck: {
    durationSeconds: 0.9,
    voices: [
      { type: 'sine', freq: [[0, 196]], gain: [[0, 0], [0.15, 0.35], [0.8, 0.2], [1, 0]] },
      { type: 'sine', freq: [[0, 185]], gain: [[0, 0], [0.2, 0.25], [1, 0]], start: 0.25 },
    ],
  },
  // Rollback: the rewind sweep. A fast fall through two octaves with a second voice a beat behind
  // it, which is what a tape running backwards sounds like without sampling one.
  rewind: {
    durationSeconds: 0.5,
    voices: [
      {
        type: 'triangle',
        freq: [
          [0, 1320],
          [1, 180],
        ],
        gain: [
          [0, 0],
          [0.06, 0.4],
          [0.7, 0.22],
          [1, 0],
        ],
      },
      {
        type: 'square',
        freq: [
          [0, 660],
          [1, 120],
        ],
        gain: [
          [0, 0],
          [0.08, 0.1],
          [1, 0],
        ],
        start: 0.12,
      },
    ],
  },
  // Day rollover: a soft stinger. Two sine notes a fifth apart at low level — structure, not an
  // achievement, so it must not sound like a rung.
  stinger: {
    durationSeconds: 0.55,
    voices: [
      { type: 'sine', freq: [[0, 392]], gain: [[0, 0], [0.12, 0.2], [1, 0]] },
      { type: 'sine', freq: [[0, 587]], gain: [[0, 0], [0.12, 0.16], [1, 0]], start: 0.18 },
    ],
  },
  // Feed stale: a two-tone alarm, the only sound that means something is wrong.
  stale: {
    durationSeconds: 0.6,
    voices: [
      { type: 'square', freq: [[0, 440]], gain: [[0, 0], [0.05, 0.22], [0.45, 0.22], [0.5, 0]] },
      { type: 'square', freq: [[0, 330]], gain: [[0, 0], [0.05, 0.22], [0.45, 0.22], [0.5, 0]], start: 0.5 },
    ],
  },
};

/** Render the whole bank into buffers. Returns an empty map if Web Audio is unusable. */
export async function renderSfxBank(sampleRate: number): Promise<Map<SfxName, AudioBuffer>> {
  const bank = new Map<SfxName, AudioBuffer>();
  if (typeof OfflineAudioContext === 'undefined') return bank;

  for (const name of SFX_NAMES) {
    try {
      const buffer = await renderOne(RECIPES[name], sampleRate);
      bank.set(name, buffer);
    } catch {
      // A missing sound effect is not worth failing a broadcast over.
    }
  }
  return bank;
}

async function renderOne(recipe: Recipe, sampleRate: number): Promise<AudioBuffer> {
  const length = Math.max(1, Math.ceil(recipe.durationSeconds * sampleRate));
  const offline = new OfflineAudioContext({ numberOfChannels: 2, length, sampleRate });

  for (const voice of recipe.voices) {
    const startAt = (voice.start ?? 0) * recipe.durationSeconds;
    const span = recipe.durationSeconds - startAt;
    if (span <= 0) continue;

    const oscillator = offline.createOscillator();
    oscillator.type = voice.type;
    const gainNode = offline.createGain();

    oscillator.frequency.setValueAtTime(voice.freq[0]?.[1] ?? 440, startAt);
    for (const [fraction, hz] of voice.freq.slice(1)) {
      oscillator.frequency.linearRampToValueAtTime(hz, startAt + fraction * span);
    }

    gainNode.gain.setValueAtTime(voice.gain[0]?.[1] ?? 0, startAt);
    for (const [fraction, value] of voice.gain.slice(1)) {
      gainNode.gain.linearRampToValueAtTime(value, startAt + fraction * span);
    }

    oscillator.connect(gainNode).connect(offline.destination);
    oscillator.start(startAt);
    oscillator.stop(recipe.durationSeconds);
  }

  return offline.startRendering();
}
