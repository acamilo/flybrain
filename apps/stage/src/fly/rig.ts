/**
 * The fly's skeleton, and the pose it takes for one frame.
 *
 * This file is the whole animal: proportions, the tripod gait, the leg IK, the wing beat and the
 * mapping from normalized neural drives to joints. It knows nothing about how it will be drawn —
 * it emits world-space points — which is what lets the WebGL renderer and the 2D "paper" fallback
 * be the *same* fly rather than two lookalikes (`docs/design/fly-avatar.md`).
 *
 * There is no Game Boy in this file. The fly used to tap one with its front legs; per review
 * ("the fly isn't really pressing buttons; just have his limbs and wings wired up to the motor
 * neurons"), the legs and wings are driven only by real population rates now, and the eight
 * button indicators are their own plain row again, above this scene (`src/panels/FlyStrip.tsx`).
 *
 * Rig space: +X right, +Y up, +Z toward the game screen, floor at y = 0, the fly's thorax over
 * the origin facing +Z. One unit is about 0.4 mm of fly, and the camera is set so the body reads
 * clearly in the fly's own share of the 800x220 strip.
 *
 * Nothing here is random and nothing is scripted: every number below is either a fixed proportion
 * or a function of `drives` and the clock. That is the design's one hard rule for this panel.
 */

export type Vec3 = [number, number, number];

/** An ellipsoid body part. */
export interface Blob {
  c: Vec3;
  /** Radii per axis. */
  r: Vec3;
}

/** A tapered limb segment between two joints. */
export interface Segment {
  a: Vec3;
  b: Vec3;
  ra: number;
  rb: number;
}

/** A flat polygon (the wings). */
export interface Poly {
  points: Vec3[];
}

/** One frame of the fly, in world space. */
export interface FlyFrame {
  head: Blob;
  eyes: [Blob, Blob];
  antennae: Segment[];
  thorax: Blob;
  abdomen: Blob[];
  /** Six legs, four joints each: coxa, trochanter, knee, tarsus tip. */
  legs: Vec3[][];
  wings: [Poly, Poly];
  halteres: [Blob, Blob];
  proboscis: Segment;
  /** Warm glow inside head and thorax, 0..1, from the PAM rate. */
  glow: number;
}

/**
 * Normalized 0..1 drives. Every one comes from the running-reference scaler in `src/fly/drives.ts`,
 * never from raw Hz.
 */
export interface FlyDrives {
  forward: number;
  backward: number;
  steerLeft: number;
  steerRight: number;
  /** Wing/flight drive: amplitude and frequency of the wing beat, and haltere jitter follow it.
   *  See `src/fly/drives.ts` for what feeds this today and why. */
  wing: number;
  proboscis: number;
  reward: number;
  population: number;
  /** 1 while a sugar moment is on screen. Extends the proboscis whatever the taste rate does. */
  sugarPulse: number;
}

/** An all-zero drive set: what the rig poses from before the first snapshot. */
export function idleDrives(): FlyDrives {
  return {
    forward: 0,
    backward: 0,
    steerLeft: 0,
    steerRight: 0,
    wing: 0,
    proboscis: 0,
    reward: 0,
    population: 0,
    sugarPulse: 0,
  };
}

// -- Proportions -------------------------------------------------------------------------------

const THORAX: Blob = { c: [0, 0.6, 0], r: [0.4, 0.36, 0.52] };
const HEAD: Blob = { c: [0, 0.68, 0.6], r: [0.3, 0.3, 0.28] };
const EYE_OFFSET = 0.25;
const EYE: Blob = { c: [0, 0.71, 0.64], r: [0.21, 0.24, 0.22] };

/** Four abdominal segments, tapering back and down. The banding is a per-segment shade. */
const ABDOMEN: readonly Blob[] = [
  { c: [0, 0.58, -0.48], r: [0.34, 0.32, 0.26] },
  { c: [0, 0.55, -0.76], r: [0.32, 0.3, 0.24] },
  { c: [0, 0.5, -1.02], r: [0.27, 0.25, 0.22] },
  { c: [0, 0.45, -1.26], r: [0.19, 0.18, 0.2] },
];

/** Where each leg leaves the thorax, and where its foot rests. Left, right, front to back. */
const COXA: readonly Vec3[] = [
  [-0.3, 0.46, 0.4],
  [0.3, 0.46, 0.4],
  [-0.34, 0.44, 0.02],
  [0.34, 0.44, 0.02],
  [-0.3, 0.44, -0.34],
  [0.3, 0.44, -0.34],
];

const STANCE: readonly Vec3[] = [
  [-0.62, 0, 0.55],
  [0.62, 0, 0.55],
  [-0.72, 0, 0.02],
  [0.72, 0, 0.02],
  [-0.66, 0, -0.62],
  [0.66, 0, -0.62],
];

/** Femur and tibia. */
const FEMUR = 0.62;
const TIBIA = 0.7;
/** The short coxa stub before the two-bone chain, which is what makes the leg three-jointed. */
const COXA_STUB = 0.14;

/** Tripod gait: legs 0, 3, 4 step together, then 1, 2, 5. */
const GAIT_OFFSET: readonly number[] = [0, 0.5, 0.5, 0, 0, 0.5];

/** Top step rate, in steps per second, at a fully-driven walk. */
const STEP_HZ = 2.2;
/** How far a foot travels fore-and-aft in one stride, and how high it lifts in swing. */
const STRIDE = 0.26;
const LIFT = 0.16;
/** Body yaw at full one-sided steering, radians (about 7 degrees). */
const YAW_MAX = 0.12;

/**
 * How much a leg's own stride lengthens or shortens per unit of differential steering.
 *
 * Turning is a differential-drive read of `steer_left`/`steer_right`: the leg on the side away
 * from the stronger steering signal is the "outer" leg and takes a longer stride, the near side
 * a shorter one — the same shape a tank uses to turn, and what makes the walk visibly lean into a
 * turn rather than just yawing the body.
 */
const STEER_STRIDE_GAIN = 0.85;
const STRIDE_SCALE_MIN = 0.15;
const STRIDE_SCALE_MAX = 1.85;

/** Wing beat, idle floor and full drive. Frequency and amplitude both ramp with `drives.wing`. */
const WING_BEAT_HZ_IDLE = 3;
const WING_BEAT_HZ_MAX = 22;
const WING_AMPLITUDE_IDLE = 0.015;
const WING_AMPLITUDE_MAX = 0.2;
/** Halteres beat antiphase to the wings (the real animal's own gyroscopic pairing), smaller. */
const HALTERE_AMPLITUDE_SCALE = 0.35;

const WING_OUTLINE: readonly Vec3[] = [
  [0.14, 0.92, -0.05],
  [0.34, 0.95, -0.55],
  [0.46, 0.94, -1.25],
  [0.3, 0.92, -1.45],
  [0.18, 0.91, -0.9],
];

const HALTERE: Blob = { c: [0.24, 0.52, -0.4], r: [0.07, 0.07, 0.07] };

const ANTENNA_BASE: Vec3 = [0.1, 0.52, 0.78];
const ANTENNA_MID: Vec3 = [0.14, 0.44, 0.9];
const ANTENNA_TIP: Vec3 = [0.13, 0.33, 0.95];

const PROBOSCIS_BASE: Vec3 = [0, 0.46, 0.74];
/** Retracted length, and how much the taste circuit (or a sugar pulse) adds. */
const PROBOSCIS_MIN = 0.1;
const PROBOSCIS_MAX = 0.52;

// -- Small vector helpers ----------------------------------------------------------------------

function sub(a: Vec3, b: Vec3): Vec3 {
  return [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
}

function add(a: Vec3, b: Vec3): Vec3 {
  return [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
}

function scale(a: Vec3, k: number): Vec3 {
  return [a[0] * k, a[1] * k, a[2] * k];
}

function length(a: Vec3): number {
  return Math.hypot(a[0], a[1], a[2]);
}

function normalize(a: Vec3): Vec3 {
  const l = length(a);
  return l < 1e-6 ? [0, 0, 0] : [a[0] / l, a[1] / l, a[2] / l];
}

function dot(a: Vec3, b: Vec3): number {
  return a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
}

function clamp01(value: number): number {
  return value < 0 ? 0 : value > 1 ? 1 : value;
}

function clampRange(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value;
}

/** Smooth 0..1 ramp, used for the proboscis extension. */
function ease(t: number): number {
  const x = clamp01(t);
  return x * x * (3 - 2 * x);
}

// -- The rig -----------------------------------------------------------------------------------

/**
 * A posed fly.
 *
 * `advance` is the only stateful call: it integrates the gait phase and the wing phase, and it
 * does so from the clock the caller passes, which on a held fixture seek is frozen. A clock that
 * has stopped (dt = 0) or jumped (dt over `PHASE_RESET_MS`, which is what a seek looks like)
 * resets both phases to zero rather than integrating across it. That is what makes a screenshot
 * of a held page reproducible: a frozen clock always poses the same fly, whatever the page did on
 * its way there.
 */
export class FlyRig {
  private phase = 0;
  private wingPhase = 0;
  private lastMs: number | null = null;

  /** Beyond this, the clock jumped (a seek, a tab wake) and the phases restart. */
  private static readonly PHASE_RESET_MS = 250;

  /** The gait phase, 0..1. Exposed for the tests. */
  get gaitPhase(): number {
    return this.phase;
  }

  reset(): void {
    this.phase = 0;
    this.wingPhase = 0;
    this.lastMs = null;
  }

  advance(drives: FlyDrives, nowMs: number): FlyFrame {
    const forward = clamp01(drives.forward);
    const backward = clamp01(drives.backward);
    const speed = Math.max(forward, backward);
    const direction = backward > forward ? -1 : 1;

    const dtMs = this.lastMs === null ? 0 : nowMs - this.lastMs;
    this.lastMs = nowMs;
    if (dtMs <= 0 || dtMs > FlyRig.PHASE_RESET_MS) {
      this.phase = 0;
      this.wingPhase = 0;
    } else {
      this.phase = (this.phase + (dtMs / 1000) * STEP_HZ * speed) % 1;
      const wingHz = WING_BEAT_HZ_IDLE + (WING_BEAT_HZ_MAX - WING_BEAT_HZ_IDLE) * clamp01(drives.wing);
      this.wingPhase = (this.wingPhase + (dtMs / 1000) * wingHz) % 1;
    }

    return this.pose(drives, nowMs, speed, direction);
  }

  private pose(drives: FlyDrives, nowMs: number, speed: number, direction: number): FlyFrame {
    const seconds = nowMs / 1000;

    // Body. Yaw toward the stronger steering side; bob with the gait; breathe with the
    // population rate (and a little even at rest, which is the design's "idle: breathing only").
    const steerDelta = clamp01(drives.steerRight) - clamp01(drives.steerLeft);
    const yaw = steerDelta * YAW_MAX;
    const bob = Math.sin(seconds * STEP_HZ * speed * 4 * Math.PI) * 0.018 * speed;
    const breathAmplitude = 0.012 + 0.05 * clamp01(drives.population);
    const breath = 1 + breathAmplitude * Math.sin(seconds * 2.4);

    const cos = Math.cos(yaw);
    const sin = Math.sin(yaw);
    /** Body space to world: yaw about Y through the origin, then the gait bob. */
    const toWorld = (p: Vec3): Vec3 => [p[0] * cos + p[2] * sin, p[1] + bob, -p[0] * sin + p[2] * cos];

    const head = { c: toWorld(HEAD.c), r: [...HEAD.r] as Vec3 };
    const thorax = { c: toWorld(THORAX.c), r: [...THORAX.r] as Vec3 };

    const eyes: [Blob, Blob] = [
      { c: toWorld([EYE.c[0] - EYE_OFFSET, EYE.c[1], EYE.c[2]]), r: [...EYE.r] as Vec3 },
      { c: toWorld([EYE.c[0] + EYE_OFFSET, EYE.c[1], EYE.c[2]]), r: [...EYE.r] as Vec3 },
    ];

    // The abdomen breathes: each segment swells about its own centre, and the whole train
    // stretches slightly, which is what reads as breathing at this size.
    const abdomen = ABDOMEN.map((segment) => ({
      c: toWorld([segment.c[0], segment.c[1], segment.c[2] * (2 - breath)]),
      r: [segment.r[0] * breath, segment.r[1] * breath, segment.r[2]] as Vec3,
    }));

    const antennae: Segment[] = [];
    for (const side of [-1, 1]) {
      const base = toWorld([ANTENNA_BASE[0] * side, ANTENNA_BASE[1], ANTENNA_BASE[2]]);
      const mid = toWorld([ANTENNA_MID[0] * side, ANTENNA_MID[1], ANTENNA_MID[2]]);
      const tip = toWorld([ANTENNA_TIP[0] * side, ANTENNA_TIP[1], ANTENNA_TIP[2]]);
      antennae.push({ a: base, b: mid, ra: 0.035, rb: 0.03 }, { a: mid, b: tip, ra: 0.03, rb: 0.055 });
    }

    // Wings: amplitude and frequency both ramp with the wing drive (`drives.wing`); an idle floor
    // keeps a small tremor rather than dead stillness. Halteres jitter antiphase, at a fraction of
    // the same amplitude — the real animal's own gyroscopic pairing.
    const wingDrive = clamp01(drives.wing);
    const wingAmplitude = WING_AMPLITUDE_IDLE + (WING_AMPLITUDE_MAX - WING_AMPLITUDE_IDLE) * wingDrive;
    const wingBeat = Math.sin(this.wingPhase * 2 * Math.PI);

    const wings = [-1, 1].map((side) => ({
      points: WING_OUTLINE.map((point) => {
        const lift = (point[2] + 0.05) * wingBeat * wingAmplitude * side;
        return toWorld([point[0] * side, point[1] + lift, point[2]]);
      }),
    })) as [Poly, Poly];

    const haltereJitter = Math.sin(this.wingPhase * 2 * Math.PI + Math.PI) * wingAmplitude * HALTERE_AMPLITUDE_SCALE;
    const halteres: [Blob, Blob] = [
      { c: toWorld([-HALTERE.c[0], HALTERE.c[1] + haltereJitter, HALTERE.c[2]]), r: [...HALTERE.r] as Vec3 },
      { c: toWorld([HALTERE.c[0], HALTERE.c[1] + haltereJitter, HALTERE.c[2]]), r: [...HALTERE.r] as Vec3 },
    ];

    // Proboscis: the taste circuit's own rate, and a sugar redemption overrides it upward.
    const extend = Math.max(clamp01(drives.proboscis), clamp01(drives.sugarPulse));
    const probBase = toWorld(PROBOSCIS_BASE);
    const probDirection = normalize(toWorld([0, -0.55, 0.84]));
    const probLength = PROBOSCIS_MIN + (PROBOSCIS_MAX - PROBOSCIS_MIN) * ease(extend);
    const proboscis: Segment = {
      a: probBase,
      b: add(probBase, scale(probDirection, probLength)),
      ra: 0.075,
      rb: 0.05,
    };

    // Legs: a tripod gait when moving, otherwise the resting stance (idle: no gait motion at
    // all, per the design's "idle: subtle breathing only" — the body's own bob and breath already
    // zero out at speed 0, so a resting leg is simply still).
    const legs: Vec3[][] = [];
    for (let leg = 0; leg < 6; leg++) {
      const coxa = toWorld(COXA[leg] as Vec3);
      const stance = toWorld(STANCE[leg] as Vec3);
      let tip = stance;

      if (speed > 0.002) {
        const side = (STANCE[leg] as Vec3)[0] < 0 ? -1 : 1;
        const strideScale = clampRange(1 - side * STEER_STRIDE_GAIN * steerDelta, STRIDE_SCALE_MIN, STRIDE_SCALE_MAX);
        const reach = STRIDE * strideScale;
        const u = (this.phase + (GAIT_OFFSET[leg] ?? 0)) % 1;
        const along = u < 0.5 ? reach * (1 - 4 * u) : reach * (4 * (u - 0.5) - 1);
        tip = [tip[0], tip[1], tip[2] + along * direction * speed];
        if (u >= 0.5) tip[1] += Math.sin(Math.PI * (u - 0.5) * 2) * LIFT * speed;
      }

      legs.push(solveLeg(coxa, tip, (STANCE[leg] as Vec3)[0] < 0 ? -1 : 1));
    }

    return {
      head,
      eyes,
      antennae,
      thorax,
      abdomen,
      legs,
      wings,
      halteres,
      proboscis,
      glow: clamp01(drives.reward),
    };
  }
}

/**
 * Two-bone IK with a short coxa stub in front of it: coxa, trochanter, knee, tarsus tip.
 *
 * The knee is placed on the side of the femur-tibia plane that points up and away from the body,
 * which is what gives a fly its high-elbow stance instead of a mammal's.
 */
export function solveLeg(coxa: Vec3, tip: Vec3, side: number): Vec3[] {
  const outward: Vec3 = normalize([side * 1, 1.25, 0]);
  const trochanter = add(coxa, scale(normalize(add(outward, sub(tip, coxa))), COXA_STUB));

  const delta = sub(tip, trochanter);
  const distance = Math.min(Math.max(length(delta), Math.abs(FEMUR - TIBIA) + 0.02), FEMUR + TIBIA - 0.02);
  const direction = normalize(delta);

  const along = (FEMUR * FEMUR - TIBIA * TIBIA + distance * distance) / (2 * distance);
  const out = Math.sqrt(Math.max(0, FEMUR * FEMUR - along * along));

  // Bend axis: `outward` with its component along the limb removed, so the knee rises sideways.
  const projected = sub(outward, scale(direction, dot(outward, direction)));
  const bend = length(projected) < 1e-4 ? ([0, 1, 0] as Vec3) : normalize(projected);

  const knee = add(add(trochanter, scale(direction, along)), scale(bend, out));
  return [coxa, trochanter, knee, [...tip] as Vec3];
}

/** Limb thickness at each joint, for whichever renderer is drawing the segments. */
export const LEG_RADII = [0.075, 0.06, 0.042, 0.022];
