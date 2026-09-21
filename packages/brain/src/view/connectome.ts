/**
 * Connectome activity viewer: an orthographic point cloud of the whole brain, additively glowing
 * wherever a neuron has just spiked, over a faint scatter of its outgoing edges.
 *
 * **This module needs a DOM and a WebGL context** (`HTMLCanvasElement`, `ResizeObserver`,
 * `requestAnimationFrame`, `fetch` + `DecompressionStream`) and it imports `three`, which is an
 * optional peer dependency. It is therefore reachable *only* through the `@flybrain/brain/view`
 * subpath export and is deliberately absent from the package entry point, so a headless trainer or
 * a server never pulls `three` or browser globals into its bundle.
 *
 * Ported from the original prototype viewer; the default classification, colours and shader
 * behaviour reproduce it exactly, and everything the prototype hard-coded is now an option.
 */
import * as THREE from 'three';
import { loadCompressed } from '../dataset/load-browser';
import { classifyByRoles, normalizePositions } from './layout';

export { classifyByRoles, normalizePositions, type RoleClassification } from './layout';

/** An `[r, g, b]` triple in the 0..1 range, as the shader consumes it. */
export type ColorTriple = [number, number, number];

/** Per-class point colours. Defaults reproduce the original viewer's shader literals. */
export interface ConnectomeColors {
  /** Class 0. Default `[0.30, 0.78, 1.0]`. */
  sensory: ColorTriple;
  /** Class 1. Default `[0.52, 0.61, 0.55]`. */
  internal: ColorTriple;
  /** Class 2. Default `[1.0, 0.68, 0.24]`. */
  output: ColorTriple;
}

export interface ConnectomeViewOptions {
  /**
   * Label every neuron 0 (sensory), 1 (internal) or 2 (output); the result must have one entry per
   * neuron. `fileClasses` is the dataset's own `classes.binz`, which the default ignores — see
   * {@link ConnectomeView.load}.
   */
  classify?: (roles: Record<string, number[]>, count: number, fileClasses: Uint8Array) => Uint8Array;
  /** Point colours per class; omitted channels fall back to the defaults above. */
  colors?: Partial<ConnectomeColors>;
  /** Milliseconds a spike takes to fade out. Default 110. */
  glowMs?: number;
  /** Opacity of the edge scatter. Default 0.045. */
  edgeOpacity?: number;
  /** Upper bound on the device pixel ratio the renderer honours. Default 2. */
  pixelRatioCap?: number;
}

const DEFAULT_COLORS: ConnectomeColors = {
  sensory: [0.30, 0.78, 1.0],
  internal: [0.52, 0.61, 0.55],
  output: [1.0, 0.68, 0.24],
};

const DEFAULT_GLOW_MS = 110;
const DEFAULT_EDGE_OPACITY = 0.045;
const DEFAULT_PIXEL_RATIO_CAP = 2;
const EDGE_COLOR = 0x536158;

/**
 * Point size grows with the glow and again for output neurons; `uGlowMs` is the fade window and
 * `uTime` the brain clock the spike times are stamped in.
 */
const POINT_VERTEX_SHADER = `
        attribute float aClass;
        attribute float aSpike;
        varying float vClass;
        varying float vGlow;
        uniform float uTime;
        uniform float uGlowMs;
        void main() {
          vClass = aClass;
          vGlow = 1.0 - smoothstep(0.0, uGlowMs, uTime - aSpike);
          vec4 mv = modelViewMatrix * vec4(position, 1.0);
           gl_PointSize = clamp((1.0 + vGlow * 3.0 + (aClass > 1.5 ? 2.0 : 0.0)) * (4.0 / -mv.z), 1.0, 8.0);
          gl_Position = projectionMatrix * mv;
        }
      `;

/** A soft round sprite, tinted by class and brightened while the neuron is glowing. */
const POINT_FRAGMENT_SHADER = `
        varying float vClass;
        varying float vGlow;
        uniform vec3 uSensory;
        uniform vec3 uInternal;
        uniform vec3 uOutput;
        void main() {
          vec2 p = gl_PointCoord - 0.5;
          float alpha = smoothstep(0.5, 0.05, length(p));
          vec3 color = vClass < 0.5 ? uSensory : (vClass > 1.5 ? uOutput : uInternal);
           color = color * (0.65 + vGlow * 0.65);
          gl_FragColor = vec4(color, alpha * (0.25 + vGlow * 0.75));
        }
      `;

export class ConnectomeView {
  private readonly renderer: THREE.WebGLRenderer;
  private readonly scene = new THREE.Scene();
  private readonly camera = new THREE.OrthographicCamera(-1.5, 1.5, 1.5, -1.5, 0.01, 100);
  private readonly classify: NonNullable<ConnectomeViewOptions['classify']>;
  private readonly colors: ConnectomeColors;
  private readonly glowMs: number;
  private readonly edgeOpacity: number;
  private material: THREE.ShaderMaterial | null = null;
  private spikeAttribute: THREE.BufferAttribute | null = null;
  private resizeObserver: ResizeObserver;
  private raf = 0;

  constructor(private readonly canvas: HTMLCanvasElement, options: ConnectomeViewOptions = {}) {
    this.classify = options.classify ?? ((roles, count) => classifyByRoles(roles, count));
    this.colors = { ...DEFAULT_COLORS, ...options.colors };
    this.glowMs = options.glowMs ?? DEFAULT_GLOW_MS;
    this.edgeOpacity = options.edgeOpacity ?? DEFAULT_EDGE_OPACITY;
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true, powerPreference: 'high-performance' });
    this.renderer.setPixelRatio(Math.min(devicePixelRatio, options.pixelRatioCap ?? DEFAULT_PIXEL_RATIO_CAP));
    this.camera.position.set(0, 0, 3.2);
    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(canvas);
    this.animate();
  }

  /**
   * Load the viewer artifacts of one dataset directory and build the scene.
   *
   * Note that the dataset's own `classes.binz` is loaded but, by default, discarded: the original
   * viewer overwrote it with a role-derived labelling (everything internal, then sensory, then
   * motor and descending), which {@link classifyByRoles} reproduces. A `classify` option receives
   * the file's classes as its third argument and may use them instead.
   */
  async load(base = '/data/fafb-v783'): Promise<void> {
    const [positions, fileClasses, edgeIndices] = await Promise.all([
      loadCompressed(`${base}/positions.binz`, (buffer) => new Float32Array(buffer)),
      loadCompressed(`${base}/classes.binz`, (buffer) => new Uint8Array(buffer)),
      loadCompressed(`${base}/viewer-edges.binz`, (buffer) => new Uint32Array(buffer)),
    ]);
    const rolesResponse = await fetch(`${base}/circuit-roles.json`);
    if (!rolesResponse.ok) throw new Error('Unable to load map circuit roles');
    const { roles } = await rolesResponse.json() as { roles: Record<string, number[]> };
    const classes = this.classify(roles, fileClasses.length, fileClasses);
    if (classes.length !== fileClasses.length) throw new Error('Neuron classification does not match connectome');
    const normalized = normalizePositions(positions);
    const spikeTimes = new Float32Array(classes.length).fill(-1_000_000);
    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute('position', new THREE.BufferAttribute(normalized, 3));
    geometry.setAttribute('aClass', new THREE.BufferAttribute(classes, 1));
    this.spikeAttribute = new THREE.BufferAttribute(spikeTimes, 1);
    geometry.setAttribute('aSpike', this.spikeAttribute);
    this.material = new THREE.ShaderMaterial({
      transparent: true,
      depthWrite: false,
      blending: THREE.AdditiveBlending,
      uniforms: {
        uTime: { value: 0 },
        uGlowMs: { value: this.glowMs },
        uSensory: { value: new THREE.Vector3(...this.colors.sensory) },
        uInternal: { value: new THREE.Vector3(...this.colors.internal) },
        uOutput: { value: new THREE.Vector3(...this.colors.output) },
      },
      vertexShader: POINT_VERTEX_SHADER,
      fragmentShader: POINT_FRAGMENT_SHADER,
    });
    this.scene.add(new THREE.Points(geometry, this.material));

    const edgePositions = new Float32Array(edgeIndices.length * 3);
    for (let i = 0; i < edgeIndices.length; i++) {
      const source = edgeIndices[i] * 3;
      edgePositions[i * 3] = normalized[source];
      edgePositions[i * 3 + 1] = normalized[source + 1];
      edgePositions[i * 3 + 2] = normalized[source + 2];
    }
    const edgeGeometry = new THREE.BufferGeometry();
    edgeGeometry.setAttribute('position', new THREE.BufferAttribute(edgePositions, 3));
    this.scene.add(new THREE.LineSegments(edgeGeometry, new THREE.LineBasicMaterial({ color: EDGE_COLOR, transparent: true, opacity: this.edgeOpacity, depthWrite: false })));
  }

  /**
   * Hand the viewer the latest per-neuron spike timestamps, stamped in the same clock as `nowMs`.
   *
   * Accepts the `Float32Array` the simulation keeps or the raw `ArrayBuffer` a worker posts, and
   * ignores a payload whose length does not match the loaded connectome.
   */
  update(spikeTimes: Float32Array | ArrayBuffer, nowMs: number): void {
    if (!this.spikeAttribute || !this.material) return;
    const incoming = spikeTimes instanceof Float32Array ? spikeTimes : new Float32Array(spikeTimes);
    if (incoming.length !== this.spikeAttribute.count) return;
    (this.spikeAttribute.array as Float32Array).set(incoming);
    this.spikeAttribute.needsUpdate = true;
    this.material.uniforms.uTime.value = nowMs;
  }

  private resize(): void {
    const width = this.canvas.clientWidth;
    const height = this.canvas.clientHeight;
    if (!width || !height) return;
    this.renderer.setSize(width, height, false);
    const aspect = width / height;
    this.camera.left = -1.45 * Math.max(1, aspect);
    this.camera.right = -this.camera.left;
    this.camera.top = 1.45 * Math.max(1, 1 / aspect);
    this.camera.bottom = -this.camera.top;
    this.camera.updateProjectionMatrix();
  }

  private animate = (): void => {
    this.raf = requestAnimationFrame(this.animate);
    this.renderer.render(this.scene, this.camera);
  };

  dispose(): void {
    cancelAnimationFrame(this.raf);
    this.resizeObserver.disconnect();
    this.renderer.dispose();
  }
}
