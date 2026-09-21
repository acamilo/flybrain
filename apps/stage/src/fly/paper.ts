/**
 * The paper fly: the same rig, projected by hand into a 2D canvas and filled flat.
 *
 * It exists for one reason, stated in the design: the capture host may have no usable WebGL, and
 * three.js has no maintained software renderer. So this is a hand-written perspective projection
 * and a painter's-algorithm fill — ellipses for the body blobs, quads for the limb segments,
 * polygons for the wings. Plainer than the WebGL fly, and the same animal in the same pose from
 * the same camera: every number it draws comes from the same `FlyFrame`.
 *
 * Shading is one directional light evaluated per shape rather than per pixel, which is what
 * "flat-shaded" means here — a low-poly look by construction rather than by a shader.
 */
import type { FlyRenderer, FlyRendererOptions } from './index';
import { CAMERA } from './camera';
import { LEG_RADII, type Blob, type FlyFrame, type Vec3 } from './rig';

/** Matches `webgl.ts`: the animal's colours, not the theme's. */
const CHITIN: Rgb = [111, 92, 56];
const CHITIN_DARK: Rgb = [74, 61, 37];
const EYE_RED: Rgb = [179, 54, 42];
const WING: Rgb = [200, 220, 234];

type Rgb = [number, number, number];

/** One thing to fill, with the view-space depth it sorts by. */
interface Shape {
  depth: number;
  kind: 'poly' | 'ellipse';
  points: number[][];
  /** Ellipse: centre x, y and the two radii. */
  ellipse?: { x: number; y: number; rx: number; ry: number };
  fill: string;
}

const LIGHT: Vec3 = [-0.55, 0.74, -0.38];

export class PaperFly implements FlyRenderer {
  readonly mode = 'paper' as const;

  private readonly ctx: CanvasRenderingContext2D;
  private readonly width: number;
  private readonly height: number;
  private readonly background: string;
  private readonly floor: string;
  private readonly accent: Rgb;

  /** Camera basis, built once: right, up, forward. */
  private readonly right: Vec3;
  private readonly up: Vec3;
  private readonly forward: Vec3;
  private readonly focal: number;

  private readonly shapes: Shape[] = [];

  constructor({ canvas, width, height, palette }: FlyRendererOptions) {
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d', { alpha: false });
    if (!ctx) throw new Error('paper fly: no 2d context');
    this.ctx = ctx;
    this.width = width;
    this.height = height;
    this.background = css(palette.panel);
    this.floor = css(palette.background);
    this.accent = [...palette.output] as Rgb;

    this.forward = normalize(sub(CAMERA.target, CAMERA.position));
    this.right = normalize(cross(this.forward, [0, 1, 0]));
    this.up = cross(this.right, this.forward);
    this.focal = 1 / Math.tan(((CAMERA.fov * Math.PI) / 180) / 2);
  }

  draw(frame: FlyFrame): void {
    const ctx = this.ctx;
    ctx.fillStyle = this.background;
    ctx.fillRect(0, 0, this.width, this.height);

    this.shapes.length = 0;

    // -- The fly --------------------------------------------------------------------------------
    for (const wing of frame.wings) this.pushPoly(wing.points, WING, 1.35, 0.3);

    for (let index = 0; index < frame.abdomen.length; index++) {
      this.pushEllipse(frame.abdomen[index] as Blob, index % 2 === 0 ? CHITIN : CHITIN_DARK, 1);
    }
    for (const haltere of frame.halteres) this.pushEllipse(haltere, CHITIN_DARK, 1);

    for (const leg of frame.legs) {
      for (let joint = 0; joint < 3; joint++) {
        this.pushLimb(
          leg[joint] as Vec3,
          leg[joint + 1] as Vec3,
          LEG_RADII[joint] as number,
          LEG_RADII[joint + 1] as number,
          joint === 2 ? CHITIN_DARK : CHITIN,
        );
      }
    }

    const glow = 1 + frame.glow * 0.9;
    this.pushEllipse(frame.thorax, mix(CHITIN, this.accent, frame.glow * 0.45), glow);
    this.pushEllipse(frame.head, mix(CHITIN, this.accent, frame.glow * 0.45), glow);
    for (const eye of frame.eyes) this.pushEllipse(eye, EYE_RED, 1.15);
    for (const segment of frame.antennae) this.pushLimb(segment.a, segment.b, segment.ra, segment.rb, CHITIN_DARK);
    this.pushLimb(frame.proboscis.a, frame.proboscis.b, frame.proboscis.ra, frame.proboscis.rb, CHITIN_DARK);

    // -- Paint, far to near ---------------------------------------------------------------------
    this.paintFloor();
    this.shapes.sort((a, b) => b.depth - a.depth);
    for (const shape of this.shapes) {
      ctx.fillStyle = shape.fill;
      ctx.beginPath();
      if (shape.kind === 'ellipse' && shape.ellipse) {
        ctx.ellipse(shape.ellipse.x, shape.ellipse.y, shape.ellipse.rx, shape.ellipse.ry, 0, 0, Math.PI * 2);
      } else {
        for (let index = 0; index < shape.points.length; index++) {
          const point = shape.points[index] as number[];
          if (index === 0) ctx.moveTo(point[0] as number, point[1] as number);
          else ctx.lineTo(point[0] as number, point[1] as number);
        }
        ctx.closePath();
      }
      ctx.fill();
    }
  }

  dispose(): void {
    // Nothing to release: a 2D context owns no GPU resources.
  }

  /** A horizon band, so the empty half of the strip is not a flat rectangle. */
  private paintFloor(): void {
    const horizon = this.project([0, 0, 40]);
    const y = horizon ? Math.max(0, Math.min(this.height, horizon[1] as number)) : this.height * 0.3;
    const gradient = this.ctx.createLinearGradient(0, y, 0, this.height);
    gradient.addColorStop(0, this.floor);
    gradient.addColorStop(1, this.background);
    this.ctx.fillStyle = gradient;
    this.ctx.fillRect(0, y, this.width, this.height - y);
  }

  /** World point to canvas pixels, or null when it is behind the camera. */
  private project(p: Vec3): [number, number, number] | null {
    const d = sub(p, CAMERA.position);
    const z = dot(d, this.forward);
    if (z <= 0.05) return null;
    const x = dot(d, this.right);
    const y = dot(d, this.up);
    const aspect = this.width / this.height;
    return [
      (0.5 + ((x * this.focal) / (aspect * z)) * 0.5) * this.width,
      (0.5 - ((y * this.focal) / z) * 0.5) * this.height,
      z,
    ];
  }

  /** Pixels per world unit at a given view depth. */
  private pixelsPerUnit(z: number): number {
    return (this.focal / z) * 0.5 * this.height;
  }

  private pushPoly(points: readonly Vec3[], colour: Rgb, shade: number, alpha = 1): void {
    const projected: number[][] = [];
    let depth = 0;
    for (const point of points) {
      const p = this.project(point);
      if (!p) return;
      projected.push([p[0], p[1]]);
      depth += p[2];
    }
    if (projected.length < 3) return;
    this.shapes.push({
      depth: depth / projected.length,
      kind: 'poly',
      points: projected,
      fill: css(scaleColour(colour, shade * faceShade(points)), alpha),
    });
  }

  private pushEllipse(blob: Blob, colour: Rgb, shade: number): void {
    const p = this.project(blob.c);
    if (!p) return;
    const scale = this.pixelsPerUnit(p[2]);
    this.shapes.push({
      depth: p[2],
      kind: 'ellipse',
      points: [],
      ellipse: {
        x: p[0],
        y: p[1],
        rx: Math.max(0.6, ((blob.r[0] + blob.r[2]) / 2) * scale),
        ry: Math.max(0.6, ((blob.r[1] + blob.r[2]) / 2) * scale),
      },
      fill: css(scaleColour(colour, shade), 1),
    });
  }

  /** A limb segment as a screen-space quad between two projected joints. */
  private pushLimb(a: Vec3, b: Vec3, ra: number, rb: number, colour: Rgb): void {
    const pa = this.project(a);
    const pb = this.project(b);
    if (!pa || !pb) return;
    const dx = (pb[0] as number) - (pa[0] as number);
    const dy = (pb[1] as number) - (pa[1] as number);
    const length = Math.hypot(dx, dy);
    if (length < 0.2) return;
    const nx = -dy / length;
    const ny = dx / length;
    const wa = Math.max(0.7, ra * this.pixelsPerUnit(pa[2]));
    const wb = Math.max(0.6, rb * this.pixelsPerUnit(pb[2]));
    this.shapes.push({
      depth: (pa[2] + pb[2]) / 2,
      kind: 'poly',
      points: [
        [pa[0] + nx * wa, pa[1] + ny * wa],
        [pb[0] + nx * wb, pb[1] + ny * wb],
        [pb[0] - nx * wb, pb[1] - ny * wb],
        [pa[0] - nx * wa, pa[1] - ny * wa],
      ],
      fill: css(scaleColour(colour, 1), 1),
    });
  }
}

/** Lambert term for a polygon, from its own winding normal. */
function faceShade(points: readonly Vec3[]): number {
  if (points.length < 3) return 1;
  const normal = normalize(cross(sub(points[1] as Vec3, points[0] as Vec3), sub(points[2] as Vec3, points[0] as Vec3)));
  return 0.55 + 0.6 * Math.abs(dot(normal, LIGHT));
}

function scaleColour(colour: Rgb, factor: number): Rgb {
  return [
    Math.max(0, Math.min(255, colour[0] * factor)),
    Math.max(0, Math.min(255, colour[1] * factor)),
    Math.max(0, Math.min(255, colour[2] * factor)),
  ];
}

function mix(a: Rgb, b: readonly number[], t: number): Rgb {
  return [
    a[0] + ((b[0] as number) - a[0]) * t,
    a[1] + ((b[1] as number) - a[1]) * t,
    a[2] + ((b[2] as number) - a[2]) * t,
  ];
}

function css(colour: readonly number[], alpha = 1): string {
  const r = Math.round(colour[0] as number);
  const g = Math.round(colour[1] as number);
  const b = Math.round(colour[2] as number);
  return alpha >= 1 ? `rgb(${r} ${g} ${b})` : `rgb(${r} ${g} ${b} / ${alpha})`;
}

function sub(a: Vec3, b: Vec3): Vec3 {
  return [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
}

function dot(a: Vec3, b: Vec3): number {
  return a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
}

function cross(a: Vec3, b: Vec3): Vec3 {
  return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
}

function normalize(a: Vec3): Vec3 {
  const l = Math.hypot(a[0], a[1], a[2]);
  return l < 1e-6 ? [0, 0, 0] : [a[0] / l, a[1] / l, a[2] / l];
}
