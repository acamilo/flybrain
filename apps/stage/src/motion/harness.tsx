/**
 * The motion harness: every moment, one at a time, over placeholder rectangles.
 *
 * Dev only. It is the entry module for `motion-harness/index.html`, which `vite dev` serves at
 * `/motion-harness/` and `vite build` never sees (the build's only input is the app's own
 * `index.html`), so nothing here can reach the broadcast bundle.
 *
 * What it is for. The effects have to be judged before they are wired into panels that are being
 * rebuilt for rail layout v2 next door: a badge fountain over a 1012x944 rectangle labelled RAIL is
 * a fair test of the fountain, and it cannot break a panel or fight another pass for the same file.
 * So this page draws the v2 regions as dashed boxes, lights the ones the active moment's recipe
 * claims, and runs the real engine over them.
 *
 * Two things it deliberately demonstrates rather than reimplements:
 *
 *   - **The paint loop.** It drives the engine through the app's own `PaintLoop`
 *     (`src/paint/loop.ts`), unmodified, via `MotionEngine.surface(ctx)`. If the engine did not fit
 *     that interface, this page would not run — which is the point of the harness existing before
 *     the wiring does.
 *   - **The React binding.** The caption text comes from `useSyncExternalStore` over
 *     `MomentQueue.subscribe` / `getSnapshot`, at snapshot rate, while every transform is mutated
 *     straight onto the DOM node inside the frame callback. That is the same 4 Hz-React /
 *     60 Hz-paint split `src/feed/store.ts` already enforces for the rest of the page.
 *
 * `window.__motion` is the control surface `tools/motion-strip.mts` drives: a virtual clock
 * (`seek`) so a screenshot lands on an exact millisecond rather than "about 300 ms", and `shoot`,
 * which resets, fires and seeks in one call.
 */
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { createRoot } from 'react-dom/client';

import { PaintLoop } from '@/paint/loop';

import { MOTION_ANCHORS, MOTION_REGIONS, recipeFor, resolveMotionColours, type MotionRegion } from './catalogue';
import { MotionEngine } from './engine';
import { EASINGS, Smoothed } from './lerp';
import { MOMENT_TYPES, momentTotalMs, type MomentTrigger, type MomentType } from './moments';
import { MAX_PARTICLES } from './particles';

/** Caption band height, matching the promoted map's band in `src/lib/geometry.ts`. */
const CAPTION_HEIGHT = 84;

/** The label each button fires, and the caption copy that goes with it. */
const DEMO_TRIGGERS: Record<MomentType, Omit<MomentTrigger, 'type'>> = {
  badge: { label: 'BOULDER BADGE', detail: '1 of 8', value: 1, intensity: 1, source: 'event' },
  milestone: { label: 'Reached: Left the bedroom', detail: 'rung 4 of 38', value: 4, intensity: 1, source: 'event' },
  rollback: { label: 'REWIND', detail: 'try 3', value: 3, intensity: 1, source: 'event' },
  sugar: { label: 'SUGAR · ada', detail: 'PAM pulse 400 ms', intensity: 1, by: 'ada', source: 'event' },
  reward: { label: 'new place', detail: '+0.05', value: 0.05, intensity: 0.35, source: 'event' },
  dayRollover: { label: 'DAY 2', intensity: 0.6, source: 'header' },
  modeChange: { label: 'BATTLE', detail: 'was OVERWORLD', intensity: 0.3, source: 'header' },
};

interface HarnessApi {
  ready: boolean;
  /** Fire one moment now. */
  fire(type: MomentType): void;
  /** Three moments at once, to watch the priority queue sort them out. */
  storm(): void;
  /** Clear the stage and put the virtual clock back to zero. */
  reset(): void;
  /** Advance the virtual clock to `tMs` since the last reset, one 60 Hz step at a time. */
  seek(tMs: number): void;
  /** Reset, fire `type` at t=0, then seek to `tMs`. One deterministic frame. */
  shoot(type: MomentType, tMs: number): void;
  /** What is on stage, for an assertion. */
  state(): { type: MomentType | null; phase: string | null; live: number; calls: number; tMs: number };
  /** Measure a full pool's tick+draw on the real canvas. */
  perf(samples?: number): { p50Ms: number; p95Ms: number; maxMs: number; live: number };
  /** Every moment type, so the tool does not hard-code the list. */
  moments: readonly MomentType[];
  /** Total on-stage time per type, so the tool can pick sensible capture times. */
  totalMs(type: MomentType): number;
}

declare global {
  // eslint-disable-next-line no-var
  interface Window {
    __motion?: HarnessApi;
  }
}

const params = new URLSearchParams(window.location.search);
const SHOW_CONTROLS = params.get('controls') !== '0';
const MANUAL = params.get('manual') === '1';

function Harness(): React.JSX.Element {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const captionRef = useRef<HTMLDivElement | null>(null);
  const badgeRef = useRef<HTMLDivElement | null>(null);
  const dayRef = useRef<HTMLDivElement | null>(null);
  const flashRef = useRef<HTMLDivElement | null>(null);
  const readoutRef = useRef<HTMLDivElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const [fit, setFit] = useState(1);

  // The clock. In manual mode the engine only ever sees the virtual time `seek` sets, which is what
  // makes a contact sheet reproducible to the millisecond.
  const clock = useRef({ virtualMs: 0 });
  const engine = useMemo(
    () =>
      new MotionEngine({
        clock: () => (MANUAL ? clock.current.virtualMs : performance.now()),
        palette: resolveMotionColours(),
      }),
    [],
  );

  const snapshot = useSyncExternalStore(
    (listener) => engine.subscribe(listener),
    () => engine.snapshot(),
  );

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    canvas.width = 1920;
    canvas.height = 1080;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    // The badge count's pop and the ticker flash are `Smoothed`, not tweens: a target held at 1 for
    // the rise window and released afterwards, with asymmetric time constants, is the catalogue's
    // "1.15x bounce" and "flashes amber 120/600 ms" with no timers involved.
    const pop = new Smoothed(0, { riseTauMs: 55, fallTauMs: 150 });
    const flash = new Smoothed(0, { riseTauMs: 40, fallTauMs: 200 });
    let lastMomentId = 0;
    let calls = 0;

    const paint = (nowMs: number, dtMs: number) => {
      engine.tick(nowMs);
      const active = engine.active(nowMs);

      if (active && active.id !== lastMomentId) lastMomentId = active.id;
      // A flash is "up for 120 ms, then down over 600": the target is *held* at 1 for the rise
      // window and released afterwards. Releasing it on the same frame it was set would only ever
      // reach one frame's worth of the rise (about a third), which is how the first contact sheet
      // came out with an invisible reward flash.
      const held = (type: MomentType, forMs: number) =>
        active?.type === type && active.elapsedMs <= forMs ? 1 : 0;
      pop.to(held('badge', 90));
      flash.to(held('reward', 120));
      pop.tick(dtMs);
      flash.tick(dtMs);

      // The caption band: in from the left on out-expo, out on the exit, nothing between.
      const caption = captionRef.current;
      if (caption) {
        const presence = active && recipeFor(active.type).caption ? active.presence : 0;
        caption.style.opacity = String(presence);
        caption.style.transform = `translate3d(${String(Math.round(-120 * (1 - presence)))}px, 0, 0)`;
      }

      const badge = badgeRef.current;
      if (badge) {
        // Out-back on the way up, so the number overshoots by about 15% and settles.
        const bounce = 1 + 0.15 * EASINGS.outBack(Math.min(1, pop.value));
        badge.style.transform = `translate3d(-50%, -50%, 0) scale(${bounce.toFixed(4)})`;
      }

      const day = dayRef.current;
      if (day) {
        const sliding = active?.type === 'dayRollover';
        const progress = sliding ? EASINGS.inOutCubic(active.elapsedMs / momentTotalMs('dayRollover')) : 0;
        day.style.opacity = sliding ? String(Math.min(1, active.presence)) : '0';
        day.style.transform = `translate3d(${String(Math.round(-700 + 1400 * progress))}px, -50%, 0)`;
      }

      const line = flashRef.current;
      if (line) line.style.background = `rgba(255, 176, 32, ${(0.55 * flash.value).toFixed(3)})`;

      ctx.clearRect(0, 0, 1920, 1080);
      calls = engine.draw(ctx);

      const readout = readoutRef.current;
      if (readout) {
        readout.textContent =
          `t ${((MANUAL ? clock.current.virtualMs : nowMs) / 1000).toFixed(2)}s   ` +
          `moment ${active ? `${active.type}/${active.phase} ${(active.presence * 100).toFixed(0)}%` : '—'}   ` +
          `particles ${String(engine.field.live)}   draws ${String(calls)}   ` +
          `dropped ${String(engine.field.dropped)}   cues ${String(engine.cueCount)}`;
      }
    };

    const loop = new PaintLoop((nowMs, dtMs, time) => {
      time('motion', () => {
        paint(nowMs, dtMs);
      });
    });
    if (!MANUAL) loop.start();

    const api: HarnessApi = {
      ready: true,
      moments: MOMENT_TYPES,
      totalMs: momentTotalMs,
      fire: (type) => {
        engine.enqueue({ type, ...DEMO_TRIGGERS[type] }, engine.active(0) ? clock.current.virtualMs : undefined);
      },
      storm: () => {
        // Deliberately out of priority order, to watch the queue reorder them.
        engine.enqueue({ type: 'reward', ...DEMO_TRIGGERS.reward });
        engine.enqueue({ type: 'sugar', ...DEMO_TRIGGERS.sugar });
        engine.enqueue({ type: 'badge', ...DEMO_TRIGGERS.badge });
      },
      reset: () => {
        engine.reset();
        clock.current.virtualMs = 0;
        lastMomentId = 0;
        pop.snap(0);
        flash.snap(0);
        ctx.clearRect(0, 0, 1920, 1080);
        paint(0, 0);
      },
      seek: (tMs) => {
        // 60 Hz steps: the same integration a live frame gets, so a seek and a watch agree.
        const step = 1000 / 60;
        while (clock.current.virtualMs + step <= tMs) {
          clock.current.virtualMs += step;
          paint(clock.current.virtualMs, step);
        }
        const rest = tMs - clock.current.virtualMs;
        if (rest > 0) {
          clock.current.virtualMs = tMs;
          paint(tMs, rest);
        }
      },
      shoot: (type, tMs) => {
        api.reset();
        engine.enqueue({ type, ...DEMO_TRIGGERS[type] }, 0);
        paint(0, 0);
        if (tMs > 0) api.seek(tMs);
      },
      state: () => {
        const active = engine.active(MANUAL ? clock.current.virtualMs : performance.now());
        return {
          type: active?.type ?? null,
          phase: active?.phase ?? null,
          live: engine.field.live,
          calls,
          tMs: MANUAL ? clock.current.virtualMs : performance.now(),
        };
      },
      perf: (samples = 200) => {
        const field = engine.field;
        const fill = () => {
          field.clear();
          field.sparks(960, 540, 200, '#ffb020', -Math.PI / 2, Math.PI / 2, { lifeMs: 1200 });
          field.streamField(MOTION_REGIONS.game, Math.PI, 150, '#4fc3f7', { lifeMs: 1200 });
          field.fountain(MOTION_REGIONS.rail, 49, '#ffb020', { lifeMs: 1200 });
          field.shockwave(MOTION_ANCHORS.badgeCount.x, MOTION_ANCHORS.badgeCount.y, '#ffb020', { lifeMs: 1200 });
        };

        fill();
        for (let i = 0; i < 60; i += 1) {
          field.tick(1);
          field.draw(ctx);
        }

        const timings: number[] = [];
        fill();
        for (let i = 0; i < samples; i += 1) {
          if (field.live < MAX_PARTICLES) fill();
          const started = performance.now();
          field.tick(1);
          field.draw(ctx);
          timings.push(performance.now() - started);
        }
        const live = field.live;
        timings.sort((a, b) => a - b);
        field.clear();
        ctx.clearRect(0, 0, 1920, 1080);
        return {
          p50Ms: timings[Math.floor(timings.length * 0.5)] ?? 0,
          p95Ms: timings[Math.floor(timings.length * 0.95)] ?? 0,
          maxMs: timings[timings.length - 1] ?? 0,
          live,
        };
      },
    };

    window.__motion = api;
    api.reset();
    document.documentElement.dataset.motionReady = '1';

    const requested = params.get('moment');
    if (requested && (MOMENT_TYPES as readonly string[]).includes(requested)) {
      api.fire(requested as MomentType);
    }

    return () => {
      loop.stop();
      delete window.__motion;
      delete document.documentElement.dataset.motionReady;
    };
  }, [engine]);

  // One transform on one element, exactly as the broadcast page scales itself: the harness is
  // authored at 1920x1080 and only ever shrunk to fit a smaller window.
  useEffect(() => {
    const measure = () => {
      setFit(Math.min(1, window.innerWidth / 1920, (window.innerHeight - (SHOW_CONTROLS ? 56 : 0)) / 1080));
    };
    measure();
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, []);

  const litRegions = new Set<MotionRegion>(snapshot.active ? recipeFor(snapshot.active.type).regions : []);
  const slot = MOTION_REGIONS.tabSlot;

  return (
    <>
      <div id="stage-fit">
        <div id="stage" ref={stageRef} style={{ transform: `scale(${String(fit)})` }} data-testid="motion-stage">
          {(Object.keys(MOTION_REGIONS) as MotionRegion[])
            // `rail` is the union of the four rail rows, so drawing it as a box as well would just
            // double every border. It still lights up, as the badge's own region.
            .filter((name) => name !== 'rail')
            .map((name) => (
              <div
                key={name}
                className="region"
                data-region={name}
                data-lit={litRegions.has(name) || (name !== 'game' && name !== 'flyStrip' && name !== 'titleStrip' && litRegions.has('rail')) ? '1' : '0'}
                style={{
                  left: MOTION_REGIONS[name].x,
                  top: MOTION_REGIONS[name].y,
                  width: MOTION_REGIONS[name].width,
                  height: MOTION_REGIONS[name].height,
                }}
              >
                {SHOW_CONTROLS ? <b>{name}</b> : null}
              </div>
            ))}

          {/* The ticker's top line, which flashes amber on a reward. */}
          <div
            ref={flashRef}
            style={{
              position: 'absolute',
              left: MOTION_REGIONS.ticker.x + 12,
              top: MOTION_REGIONS.ticker.y + 12,
              width: MOTION_REGIONS.ticker.width - 24,
              height: 34,
            }}
          />

          {/* The badge count. */}
          <div id="badge-count" ref={badgeRef} style={{ left: MOTION_ANCHORS.badgeCount.x, top: MOTION_ANCHORS.badgeCount.y }}>
            1/8
          </div>

          {/* DAY N, sliding across the title strip. */}
          <div
            id="day-slide"
            ref={dayRef}
            style={{ left: MOTION_REGIONS.titleStrip.x + MOTION_REGIONS.titleStrip.width / 2, top: MOTION_REGIONS.titleStrip.y + 20 }}
          >
            {snapshot.active?.type === 'dayRollover' ? snapshot.active.label : 'DAY 2'}
          </div>

          {/* The caption band, inside the tab slot's own bottom edge. */}
          <div
            id="caption"
            ref={captionRef}
            style={{
              left: slot.x,
              top: slot.y + slot.height - CAPTION_HEIGHT,
              width: slot.width,
              height: CAPTION_HEIGHT,
              opacity: 0,
            }}
          >
            <span className="kind">{(snapshot.active?.type ?? '').toUpperCase()}</span>
            <span className="label">{snapshot.active?.label ?? ''}</span>
            <span className="detail">
              {snapshot.active?.detail ?? ''}
              {snapshot.active && snapshot.active.count > 1 ? ` ×${String(snapshot.active.count)}` : ''}
            </span>
          </div>

          <canvas id="particles" ref={canvasRef} />
        </div>
      </div>

      {SHOW_CONTROLS ? (
        <div id="controls">
          {MOMENT_TYPES.map((type) => (
            <button key={type} type="button" onClick={() => window.__motion?.fire(type)}>
              {type}
            </button>
          ))}
          <button type="button" onClick={() => window.__motion?.storm()}>
            storm (reward+sugar+badge)
          </button>
          <button type="button" data-kind="danger" onClick={() => window.__motion?.reset()}>
            reset
          </button>
          <button
            type="button"
            onClick={() => {
              const result = window.__motion?.perf();
              if (result) {
                // eslint-disable-next-line no-console
                console.log(
                  `400-particle tick+draw on canvas: p50 ${result.p50Ms.toFixed(3)} ms, p95 ${result.p95Ms.toFixed(3)} ms, max ${result.maxMs.toFixed(3)} ms`,
                );
              }
            }}
          >
            perf (console)
          </button>
          <div id="readout" ref={readoutRef} />
        </div>
      ) : (
        <div id="readout" ref={readoutRef} hidden />
      )}
    </>
  );
}

const host = document.getElementById('root');
if (host) createRoot(host).render(<Harness />);
