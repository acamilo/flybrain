/**
 * The fly in three.js: one WebGL context, low-poly primitives, flat shading.
 *
 * Everything is allocated once in the constructor and only transformed afterwards, because this
 * runs 30 times a second forever on a machine that is also running an emulator, a simulation and
 * an encoder. No geometry is rebuilt per frame; the only per-frame writes are positions, scales,
 * quaternions, one emissive colour and the wings' six vertices.
 *
 * There is no Game Boy mesh here: the fly's legs and wings are driven by motor rates, not by
 * button taps, and the eight button indicators are their own plain DOM row above this canvas
 * (`src/panels/FlyStrip.tsx`). What's left is well under the design's 3,000-triangle budget.
 * Materials are Lambert rather than Standard: a physically-based material costs far more in a
 * software rasteriser and buys nothing at this size.
 */
import {
  AmbientLight,
  BufferAttribute,
  BufferGeometry,
  Color,
  CylinderGeometry,
  DirectionalLight,
  DoubleSide,
  DynamicDrawUsage,
  Mesh,
  MeshBasicMaterial,
  MeshLambertMaterial,
  PerspectiveCamera,
  PlaneGeometry,
  Quaternion,
  Scene,
  SphereGeometry,
  Vector3,
  WebGLRenderer,
} from 'three';

import type { FlyRenderer, FlyRendererOptions } from './index';
import { CAMERA, FLOOR } from './camera';
import { LEG_RADII, type Blob, type FlyFrame, type Segment, type Vec3 } from './rig';

/** The fly's own colours. These are the animal, not the theme, so they do not move with `?theme=`. */
const CHITIN = 0x6f5c38;
const CHITIN_DARK = 0x4a3d25;
const EYE_RED = 0xb3362a;
const WING = 0xc8dcea;

const UP = new Vector3(0, 1, 0);

export class WebglFly implements FlyRenderer {
  readonly mode = 'webgl' as const;

  private readonly renderer: WebGLRenderer;
  private readonly scene = new Scene();
  private readonly camera: PerspectiveCamera;

  private readonly head: Mesh;
  private readonly thorax: Mesh;
  private readonly eyes: Mesh[] = [];
  private readonly abdomen: Mesh[] = [];
  private readonly halteres: Mesh[] = [];
  private readonly legSegments: Mesh[] = [];
  private readonly antennaSegments: Mesh[] = [];
  private readonly proboscis: Mesh;
  private readonly wings: { mesh: Mesh; position: BufferAttribute }[] = [];

  private readonly glowMaterial: MeshLambertMaterial;
  private readonly accent: Color;

  private readonly scratchA = new Vector3();
  private readonly scratchB = new Vector3();
  private readonly scratchDirection = new Vector3();
  private readonly scratchQuaternion = new Quaternion();

  constructor({ canvas, width, height, palette }: FlyRendererOptions) {
    this.renderer = new WebGLRenderer({ canvas, antialias: false, alpha: false, powerPreference: 'low-power' });
    this.renderer.setPixelRatio(1);
    this.renderer.setSize(width, height, false);
    this.renderer.setClearColor(new Color(rgb(palette.panel)), 1);

    this.camera = new PerspectiveCamera(CAMERA.fov, width / height, CAMERA.near, CAMERA.far);
    this.camera.position.set(...CAMERA.position);
    this.camera.lookAt(new Vector3(...CAMERA.target));

    this.accent = new Color(rgb(palette.output));

    this.scene.add(new AmbientLight(0x5a6272, 1.5));
    const key = new DirectionalLight(0xfff0d6, 1.7);
    key.position.set(-2.5, 4, -1.5);
    this.scene.add(key);

    // The floor: built once, never touched again.
    const floor = new Mesh(
      new PlaneGeometry(FLOOR.halfWidth * 2, FLOOR.farZ - FLOOR.nearZ),
      new MeshBasicMaterial({ color: new Color(rgb(palette.background)) }),
    );
    floor.rotation.x = -Math.PI / 2;
    floor.position.set(0, 0, (FLOOR.farZ + FLOOR.nearZ) / 2);
    this.scene.add(floor);

    // -- The fly --------------------------------------------------------------------------------
    const sphere = new SphereGeometry(1, 8, 6);
    this.glowMaterial = new MeshLambertMaterial({ color: CHITIN, flatShading: true });
    const shell = new MeshLambertMaterial({ color: CHITIN, flatShading: true });
    const band = new MeshLambertMaterial({ color: CHITIN_DARK, flatShading: true });

    this.head = new Mesh(sphere, this.glowMaterial);
    this.thorax = new Mesh(sphere, this.glowMaterial);
    this.scene.add(this.head, this.thorax);

    const eyeMaterial = new MeshLambertMaterial({ color: EYE_RED, flatShading: true });
    for (let index = 0; index < 2; index++) {
      const eye = new Mesh(sphere, eyeMaterial);
      this.eyes.push(eye);
      this.scene.add(eye);
    }

    // Four segments, alternating shade: that alternation is the abdomen's banding.
    for (let index = 0; index < 4; index++) {
      const segment = new Mesh(sphere, index % 2 === 0 ? shell : band);
      this.abdomen.push(segment);
      this.scene.add(segment);
    }

    for (let index = 0; index < 2; index++) {
      const haltere = new Mesh(sphere, band);
      this.halteres.push(haltere);
      this.scene.add(haltere);
    }

    // Legs: three tapered segments each, their radii fixed per joint so only the length changes.
    for (let leg = 0; leg < 6; leg++) {
      for (let joint = 0; joint < 3; joint++) {
        const mesh = new Mesh(
          new CylinderGeometry(LEG_RADII[joint + 1] as number, LEG_RADII[joint] as number, 1, 6),
          joint === 2 ? band : shell,
        );
        this.legSegments.push(mesh);
        this.scene.add(mesh);
      }
    }

    for (let index = 0; index < 4; index++) {
      const mesh = new Mesh(new CylinderGeometry(index % 2 === 0 ? 0.03 : 0.055, 0.035, 1, 6), band);
      this.antennaSegments.push(mesh);
      this.scene.add(mesh);
    }

    this.proboscis = new Mesh(new CylinderGeometry(0.05, 0.075, 1, 6), band);
    this.scene.add(this.proboscis);

    const wingMaterial = new MeshBasicMaterial({ color: WING, transparent: true, opacity: 0.24, side: DoubleSide });
    for (let index = 0; index < 2; index++) {
      const geometry = new BufferGeometry();
      const position = new BufferAttribute(new Float32Array(9 * 3), 3);
      position.setUsage(DynamicDrawUsage);
      geometry.setAttribute('position', position);
      const mesh = new Mesh(geometry, wingMaterial);
      mesh.frustumCulled = false;
      this.wings.push({ mesh, position });
      this.scene.add(mesh);
    }
  }

  draw(frame: FlyFrame): void {
    placeBlob(this.head, frame.head);
    placeBlob(this.thorax, frame.thorax);
    for (let index = 0; index < 2; index++) placeBlob(this.eyes[index] as Mesh, frame.eyes[index] as Blob);
    for (let index = 0; index < frame.abdomen.length; index++) {
      placeBlob(this.abdomen[index] as Mesh, frame.abdomen[index] as Blob);
    }
    for (let index = 0; index < 2; index++) placeBlob(this.halteres[index] as Mesh, frame.halteres[index] as Blob);

    for (let leg = 0; leg < frame.legs.length; leg++) {
      const joints = frame.legs[leg] as Vec3[];
      for (let joint = 0; joint < 3; joint++) {
        this.placeSegment(this.legSegments[leg * 3 + joint] as Mesh, joints[joint] as Vec3, joints[joint + 1] as Vec3);
      }
    }

    for (let index = 0; index < this.antennaSegments.length; index++) {
      const segment = frame.antennae[index] as Segment;
      this.placeSegment(this.antennaSegments[index] as Mesh, segment.a, segment.b);
    }
    this.placeSegment(this.proboscis, frame.proboscis.a, frame.proboscis.b);

    for (let index = 0; index < 2; index++) {
      const wing = this.wings[index];
      if (!wing) continue;
      const points = frame.wings[index]?.points ?? [];
      // A triangle fan over the outline, written straight into the attribute.
      let cursor = 0;
      for (let corner = 1; corner + 1 < points.length; corner++) {
        cursor = writePoint(wing.position, cursor, points[0] as Vec3);
        cursor = writePoint(wing.position, cursor, points[corner] as Vec3);
        cursor = writePoint(wing.position, cursor, points[corner + 1] as Vec3);
      }
      wing.position.needsUpdate = true;
    }

    // Head and thorax warm with the PAM rate. Emissive, not colour, so the shading survives.
    this.glowMaterial.emissive.copy(this.accent).multiplyScalar(frame.glow * 0.45);

    this.renderer.render(this.scene, this.camera);
  }

  /** Stretch a unit-height cylinder between two joints. */
  private placeSegment(mesh: Mesh, a: Vec3, b: Vec3): void {
    this.scratchA.set(a[0], a[1], a[2]);
    this.scratchB.set(b[0], b[1], b[2]);
    this.scratchDirection.subVectors(this.scratchB, this.scratchA);
    const length = this.scratchDirection.length();
    if (length < 1e-5) {
      mesh.visible = false;
      return;
    }
    mesh.visible = true;
    this.scratchDirection.divideScalar(length);
    mesh.position.copy(this.scratchA).addScaledVector(this.scratchDirection, length / 2);
    mesh.quaternion.copy(this.scratchQuaternion.setFromUnitVectors(UP, this.scratchDirection));
    mesh.scale.set(1, length, 1);
  }

  dispose(): void {
    this.renderer.dispose();
  }
}

function placeBlob(mesh: Mesh, blob: Blob): void {
  mesh.position.set(blob.c[0], blob.c[1], blob.c[2]);
  mesh.scale.set(blob.r[0], blob.r[1], blob.r[2]);
}

function writePoint(attribute: BufferAttribute, cursor: number, point: Vec3): number {
  attribute.setXYZ(cursor, point[0], point[1], point[2]);
  return cursor + 1;
}

/** `[r, g, b]` 0-255 to the 0xrrggbb three.js wants. */
function rgb(color: readonly [number, number, number]): number {
  return (Math.round(color[0]) << 16) | (Math.round(color[1]) << 8) | Math.round(color[2]);
}
