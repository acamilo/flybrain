/**
 * The body-face comparison frame: `mockups/gameboy-fonts.png`.
 *
 * `docs/design/gameboy-theme.md`'s gate says "the operator picks the body font from three candidates
 * rendered in the same frame". Three copies of the SENSES tab side by side do not fit 1920 px —
 * the rail alone is 1012 — so the frame is three *stacked strips*, each carrying the parts of the
 * rail where the body face actually has to work:
 *
 *   - the progress cluster: the 30 px rung line, the 38-cell spine, the 39 px label-lg readouts
 *     and the 27 px footer counters;
 *   - two ticker lines at the 27 px body floor;
 *   - two chat lines at the same floor, one of them a bot line.
 *
 * Every size is the real one from `src/theme/tokens.css` (27 body, 33/39 labels, 30 rung line), so
 * what the frame shows is the face at the floors it has to survive, not at a poster size. The
 * structure around it is the Game Boy dialogue box the same document asks for — square corners, a
 * 4 px outer border with a 2 px inner line, no radius, no shadow — because a face has to be judged
 * in the chrome it will sit in.
 *
 * The strip labels are in Press Start 2P, deliberately not in the candidate: the label is the one
 * piece of text in the frame that must read identically across the three strips.
 *
 * ```sh
 * npm run fonts -w @flybrain/stage       # writes mockups/gameboy-fonts.png
 * FONT_COMPARE_PORT=4602 npm run fonts -w @flybrain/stage
 * ```
 */
import { createServer, type Server } from 'node:http';
import { createReadStream } from 'node:fs';
import { mkdir, stat } from 'node:fs/promises';
import { dirname, join, normalize, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { chromium } from '@playwright/test';

const here = dirname(fileURLToPath(import.meta.url));
const appDir = resolve(here, '..');
const publicDir = resolve(appDir, 'public');
const outDir = resolve(appDir, 'mockups');

/** 4600+, per the run instructions; `FONT_COMPARE_PORT` overrides for a busy machine. */
const PORT = Number(process.env.FONT_COMPARE_PORT ?? 4601);

/**
 * The three OFL candidates `docs/design/gameboy-theme.md` names, in the order it names them.
 *
 * `picked` is the decision, not a preference: the operator chose Silkscreen from this frame on
 * 2026-09-15, and the frame says so rather than going on looking like an open question a year
 * from now. The tool keeps rendering all three because the comparison is the artifact — what
 * changed is that one strip is now labelled as the answer.
 */
const CANDIDATES = [
  { name: 'VT323', family: 'VT323', file: 'VT323-Latin.woff2', picked: false },
  { name: 'SILKSCREEN', family: 'Silkscreen', file: 'Silkscreen-Latin.woff2', picked: true },
  { name: 'PIXELIFY SANS', family: 'Pixelify Sans', file: 'PixelifySans-Latin.woff2', picked: false },
] as const;

/** One rail strip's worth of markup, in whichever face the strip declares. */
function strip(label: string, family: string, picked = false): string {
  const cells = Array.from({ length: 38 }, (_, index) => {
    const state = index < 12 ? 'reached' : index === 12 ? 'current' : 'future';
    return `<i class="cell" data-state="${state}"></i>`;
  }).join('');

  return `
<section class="strip" style="--font-body: '${family}', monospace">
  <div class="rail">
    <div class="box cluster">
      <div class="grid">
        <span class="title">rung</span>
        <span class="title">here for</span>
        <span class="title">brain</span>
        <span class="rung-line">
          <span class="rung-line__count">12/37</span>
          <span class="rung-line__name">Mt. Moon</span>
          <span class="rung-line__arrow" aria-hidden></span>
          <span class="rung-line__next">Cerulean City</span>
        </span>
        <span class="label-lg">2h41m</span>
        <span class="label-lg accent">13.5 Hz</span>
      </div>
      <div class="spine">${cells}</div>
      <div class="footer">
        <span class="body dim">1/8 badges &middot; 214 places</span>
        <span class="body dim push">try 2</span>
        <span class="body dim">06:12:33 &middot; day 3</span>
        <span class="chip"><span class="chip__ring"></span><span class="body pink">SUGAR READY</span></span>
      </div>
    </div>

    <div class="box events">
      <div class="row" data-tier="moment">
        <span class="dot"></span><span class="body">BOULDER BADGE</span><span class="body dim push">+1.00</span>
      </div>
      <div class="row" data-tier="notable">
        <span class="dot"></span><span class="body">new area Viridian Forest</span><span class="body dim push">+0.20</span>
      </div>
    </div>

    <div class="box chat">
      <div class="line"><span class="body name">ada_lovelace</span><span class="body">go left you silly fly</span></div>
      <div class="line" data-bot="1"><span class="body name">flybridge</span><span class="body">sugar is on cooldown for 41s</span></div>
    </div>
  </div>

  <span class="strip-label">${label}${picked ? '<i class="picked">picked</i>' : ''}</span>
</section>`;
}

const page = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Game Boy theme: body face candidates</title>
<style>
@font-face { font-family: 'Press Start 2P'; font-display: block; src: url('/fonts/PressStart2P-Latin.woff2') format('woff2'); }
${CANDIDATES.map(
  (candidate) =>
    `@font-face { font-family: '${candidate.family}'; font-display: block; src: url('/fonts/${candidate.file}') format('woff2'); }`,
).join('\n')}

:root {
  /* The four-step palette of docs/design/gameboy-theme.md, under T1's amber. */
  --ground: #0b0e12;
  --dark: #1e232a;
  --mid: #4a5460;
  --light: #c9d1c8;
  --amber: #f0a72e;
  --dopamine: #f472a8;
  --ok: #58d68d;
}

* { box-sizing: border-box; }

html, body {
  margin: 0;
  width: 1920px;
  height: 1080px;
  overflow: hidden;
  background: var(--ground);
  color: var(--light);
}

.frame { display: flex; flex-direction: column; gap: 6px; padding: 6px 0; height: 1080px; }

.strip {
  position: relative;
  display: flex;
  align-items: center;
  height: 352px;
  padding: 0 48px;
  font-family: var(--font-body);
  /* Pixelify Sans ships an f-l ligature that reads as a capital A at these sizes ("silly fly"
     came out "silly Ay"), and a readout page wants no ligatures in any case. Off for all three,
     so the comparison is of the letterforms rather than of one face's liga table. */
  font-variant-ligatures: none;
}

.rail { display: flex; flex-direction: column; gap: 12px; width: 1012px; }

/* The dialogue box: square corners, 4 px outer border, 2 px inner line, no shadow, no radius. */
.box {
  position: relative;
  background: var(--dark);
  border: 4px solid var(--light);
  border-radius: 0;
  padding: 5px 12px;
}
.box::after {
  content: '';
  position: absolute;
  inset: 0;
  border: 2px solid var(--mid);
  pointer-events: none;
}

.cluster { height: 144px; display: flex; flex-direction: column; justify-content: space-between; gap: 3px; }
.events { height: 100px; display: flex; flex-direction: column; justify-content: space-between; }
.chat { height: 72px; display: flex; flex-direction: column; justify-content: space-between; }

.grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto auto;
  column-gap: 22px;
  row-gap: 2px;
  align-items: baseline;
}

.title { font-size: 27px; line-height: 1.1; letter-spacing: 0.08em; text-transform: uppercase; color: var(--mid); }
.body { font-size: 27px; line-height: 1.03; }
.label-lg { font-size: 39px; line-height: 1.1; color: var(--light); }
.dim { color: var(--mid); }
.accent { color: var(--amber); }
.pink { color: var(--dopamine); }
.push { margin-left: auto; }

.rung-line { display: flex; align-items: baseline; gap: 10px; min-width: 0; font-size: 30px; line-height: 1.15; }
.rung-line__count { color: var(--mid); }
.rung-line__name { color: var(--amber); }
.rung-line__next { color: var(--light); }

/* The arrow is drawn, not typed: none of the three candidates carries U+2192. */
.rung-line__arrow {
  align-self: center;
  width: 12px;
  height: 16px;
  background: var(--mid);
  clip-path: polygon(0 0, 100% 50%, 0 100%);
}

/* Square cells, 2 px gaps, 12 px tall. */
.spine { display: flex; gap: 2px; height: 12px; }
.cell { flex: 1 1 0; background: color-mix(in srgb, var(--mid) 45%, var(--ground)); }
.cell[data-state='reached'] { background: var(--amber); }
.cell[data-state='current'] { background: var(--light); min-width: 20px; }

.footer { display: flex; align-items: center; gap: 18px; }

.chip { display: flex; align-items: center; gap: 8px; margin-left: auto; }
.chip__ring { width: 20px; height: 20px; border: 4px solid var(--ok); }

.row { display: flex; align-items: center; gap: 12px; }
.dot { width: 14px; height: 14px; background: var(--mid); }
.row[data-tier='notable'] .dot { background: var(--amber); }
.row[data-tier='moment'] .dot { background: var(--amber); width: 18px; height: 18px; }
.row[data-tier='moment'] .body { color: var(--amber); }

.line { display: flex; align-items: baseline; gap: 10px; }
.name { color: var(--amber); }
.line[data-bot='1'] .name, .line[data-bot='1'] .body { color: var(--ok); }

.strip-label {
  position: absolute;
  right: 48px;
  top: 50%;
  transform: translateY(-50%);
  font-family: 'Press Start 2P', monospace;
  font-size: 39px;
  line-height: 1.3;
  color: var(--amber);
  text-align: right;
}

/* The decision, under the name of the face that won it. */
.picked {
  display: block;
  margin-top: 8px;
  font-size: 27px;
  font-style: normal;
  color: var(--light);
}
</style>
</head>
<body>
<div class="frame">
${CANDIDATES.map((candidate) => strip(candidate.name, candidate.family, candidate.picked)).join('\n')}
</div>
</body>
</html>`;

/** Serve the generated page at `/` and `public/` underneath it. Nothing else is needed. */
function serve(): Server {
  const server = createServer((request, response) => {
    const path = (request.url ?? '/').split('?')[0] ?? '/';
    if (path === '/' || path === '/index.html') {
      response.setHeader('content-type', 'text/html; charset=utf-8');
      response.end(page);
      return;
    }

    const relative = normalize(decodeURIComponent(path).slice(1));
    if (!relative || relative.startsWith('..')) {
      response.statusCode = 403;
      response.end('forbidden');
      return;
    }

    const file = join(publicDir, relative);
    stat(file)
      .then((info) => {
        if (!info.isFile()) throw new Error('not a file');
        if (file.endsWith('.woff2')) response.setHeader('content-type', 'font/woff2');
        response.setHeader('content-length', String(info.size));
        createReadStream(file).pipe(response);
      })
      .catch(() => {
        response.statusCode = 404;
        response.end('not found');
      });
  });
  server.listen(PORT, '127.0.0.1');
  return server;
}

async function main(): Promise<void> {
  await mkdir(outDir, { recursive: true });
  const server = serve();

  try {
    const browser = await chromium.launch({ args: ['--disable-lcd-text'] });
    const context = await browser.newContext({
      viewport: { width: 1920, height: 1080 },
      deviceScaleFactor: 1,
    });
    const shot = await context.newPage();
    await shot.goto(`http://127.0.0.1:${PORT}/`);
    await shot.evaluate(() => document.fonts.ready);

    // Confirm the frame is drawing the three candidates and not a fallback: each strip's own
    // family has to be loaded, and the 27 px body line has to measure differently in each.
    const check = await shot.evaluate(() => {
      const strips = [...document.querySelectorAll('.strip')];
      return strips.map((element) => {
        const body = element.querySelector('.chat .body') as HTMLElement;
        const family = getComputedStyle(element).fontFamily.split(',')[0]?.replace(/['"]/g, '') ?? '?';
        return {
          family,
          loaded: document.fonts.check(`27px "${family}"`),
          width: Math.round(body.getBoundingClientRect().width),
        };
      });
    });
    for (const row of check) {
      if (!row.loaded) throw new Error(`${row.family} did not load — the frame would show a fallback`);
      console.log(`  ${row.family.padEnd(14)} loaded, 27 px chat line measures ${row.width} px`);
    }
    if (new Set(check.map((row) => row.width)).size !== check.length) {
      throw new Error('two strips measure identically, so at least one is rendering in the wrong face');
    }

    const path = resolve(outDir, 'gameboy-fonts.png');
    await shot.screenshot({ path, clip: { x: 0, y: 0, width: 1920, height: 1080 } });
    console.log(`\n  ${path}`);

    await browser.close();
  } finally {
    server.close();
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
