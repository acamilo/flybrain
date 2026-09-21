/**
 * The one camera both renderers use, so the paper fly and the WebGL fly frame the same shot.
 *
 * Behind and above the fly at about 30 degrees, looking over its head toward the game screen —
 * the design's words. There is no 3D Game Boy in this scene any more (the button row is its own
 * plain strip, `src/panels/FlyStrip.tsx`), so the shot is the fly and the floor alone, pulled in
 * closer than the old framing: same angle, same side, just nearer, which is what makes the fly
 * "a little larger now that the strip is its own."
 */
import type { Vec3 } from './rig';

export const CAMERA = {
  position: [0, 3.6, -4.7] as Vec3,
  target: [0, 0.45, 0.8] as Vec3,
  /** Vertical field of view, degrees. */
  fov: 25,
  near: 0.1,
  far: 40,
} as const;

/** Floor plane the fly stands on, drawn as a quiet gradient rather than a lit surface. */
export const FLOOR = { halfWidth: 12, nearZ: -6, farZ: 9 } as const;
