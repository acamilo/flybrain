/**
 * Feed rates to fly drives.
 *
 * Every value the fly moves on is a fraction of that role's *own* running reference
 * (`src/lib/circuit-scale.ts`), exactly as a circuit bar is: the page never sees the decoder's
 * calibration baseline, and a fixed Hz ceiling is what pegged every bar at 100% on the first live
 * run. The design says it in one line — "Every value is normalized the same way the circuits panel
 * does (running reference per role), never hard-coded Hz" — and this file is the whole of it.
 *
 * The population rate gets the same treatment through its own reference, which `FeedIngest`
 * advances per snapshot alongside the bar roles.
 */
import { GAMEBOY_BUTTONS } from '@flybrain/brain';

import { hot } from '@/feed/store';
import { circuitFraction, DEFAULT_HEADROOM } from '@/lib/circuit-scale';
import type { FlyDrives } from './rig';

/** Command roles in `GAMEBOY_BUTTONS` order: `command_0` is up, `command_7` is Select. */
const COMMAND_ROLES = GAMEBOY_BUTTONS.map((_, index) => `command_${index}`);

/**
 * The wing/flight drive's source roles — the one table `docs/design/fly-avatar.md` asks this
 * file to keep, so swapping the source later is a one-line change.
 *
 * The honest source is a `motor` role rate (110 neurons in `data/fafb-v783/meta.json`), but
 * `docs/feed-protocol.md`'s `rates` does not carry `motor` today — only the tracked roles it
 * lists, `command_0..7` among them. Those eight *are* descending motor commands (their neuron
 * counts sum to exactly the dataset's `descending` total), so their sum stands in for the wing
 * drive until the feed grows a `motor` rate.
 *
 * TODO(motor-in-feed): once `rates.motor` exists, change this to `['motor']`. `sumDrive` below
 * scores a sum of one role the same way it scores eight, so nothing else in this file changes.
 */
export const WING_DRIVE_ROLES: readonly string[] = COMMAND_ROLES;

/**
 * What "resting" reads as on the circuit scale.
 *
 * A role sitting exactly at its own running reference scores `1 / headroom`, about 0.67 — that
 * headroom is deliberate, so a burst still has somewhere to go. A *bar* can sit at two thirds
 * forever and look right; a fly cannot. Left raw, the proboscis would be half out and the head
 * half lit at all times, which is neither honest nor what the design asks for ("above resting it
 * walks in place"; "idle: subtle breathing only").
 *
 * So every drive is re-centred on that resting level: zero at or below its own recent normal,
 * rising to one as the rate reaches the top of its own scale. It is the same running reference,
 * read as a deviation rather than as a level.
 */
const RESTING = 1 / DEFAULT_HEADROOM;

function aboveResting(fraction: number): number {
  return fraction <= RESTING ? 0 : (fraction - RESTING) / (1 - RESTING);
}

/** One role's rate as a 0..1 drive: how far above its own resting level it is running. */
function drive(role: string): number {
  const reference = hot.circuitReferenceHz[role];
  if (reference === undefined) return 0;
  return aboveResting(circuitFraction(hot.rates[role] ?? 0, reference));
}

/**
 * Several roles' rates and references summed before scoring, so the group reads as one drive
 * against the sum of its own recent normals — the same formula `drive` uses for a single role,
 * which is what makes `WING_DRIVE_ROLES` a one-line swap later.
 */
function sumDrive(roles: readonly string[]): number {
  let rateSum = 0;
  let referenceSum = 0;
  for (const role of roles) {
    rateSum += hot.rates[role] ?? 0;
    referenceSum += hot.circuitReferenceHz[role] ?? 0;
  }
  if (referenceSum <= 0) return 0;
  return aboveResting(circuitFraction(rateSum, referenceSum));
}

/**
 * Read one frame of drives out of the hot store.
 *
 * No button state to time here any more — the button row's own afterglow is painted straight
 * from `hot.buttonStates` in `src/App.tsx`, off the page's own clock.
 */
export function readFlyDrives(sugarPulse: number, out: FlyDrives): FlyDrives {
  out.forward = drive('forward');
  out.backward = drive('backward');
  out.steerLeft = drive('steer_left');
  out.steerRight = drive('steer_right');
  out.wing = sumDrive(WING_DRIVE_ROLES);
  out.proboscis = drive('proboscis');
  out.reward = drive('reward_pam');
  out.population = aboveResting(circuitFraction(hot.populationRate, hot.populationReferenceHz));
  out.sugarPulse = sugarPulse;
  return out;
}
