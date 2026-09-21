/**
 * Playwright against the real build, at the real broadcast size: 1920x1080, DPR 1.
 *
 * Not the dev server: the page that goes on air is the `vite build` output served by `vite
 * preview`, and the differences that matter (asset URLs, the worklet module, the worker chunk,
 * no HMR client) all live in the build.
 *
 * The Chromium flags mirror what `flystage`'s systemd unit must use, per the two corrections at
 * the top of the design document: keep `--autoplay-policy=no-user-gesture-required`, drop
 * `--mute-audio` (the page is the stream's audio source), and drop every SwiftShader and ANGLE
 * flag, so the browser picks its own GL path exactly as the capture host will.
 *
 * There is now exactly one intentional GL context on the page — the fly
 * (`docs/design/fly-avatar.md`) — and `tests/e2e/structure.spec.ts` asserts it is exactly one with
 * `?fly=webgl` and none at all with `?fly=paper` or `?fly=off`. The brain map is still 2D canvas.
 */
import { defineConfig, devices } from '@playwright/test';

/**
 * Preview port, 4300 unless `FLYSTAGE_E2E_PORT` says otherwise.
 *
 * Overridable because `--strictPort` means two suites on one machine collide, and two of them at
 * once is normal here: a second worktree running the same suite, or a dev preview already holding
 * 4300. `FLYSTAGE_E2E_PORT=4400 npm run test:e2e` is the whole workaround.
 */
const PORT = Number(process.env.FLYSTAGE_E2E_PORT ?? 4300);

export default defineConfig({
  testDir: './tests/e2e',
  outputDir: './test-results',
  snapshotPathTemplate: '{testDir}/__screenshots__/{arg}{ext}',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 90_000,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : [['list']],

  expect: {
    toHaveScreenshot: {
      maxDiffPixelRatio: 0.002,
      animations: 'disabled',
    },
  },

  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    viewport: { width: 1920, height: 1080 },
    deviceScaleFactor: 1,
    trace: 'retain-on-failure',
    launchOptions: {
      args: ['--autoplay-policy=no-user-gesture-required', '--disable-lcd-text'],
    },
  },

  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'], viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 },
    },
  ],

  webServer: {
    command: `npm run build && npx vite preview --host 127.0.0.1 --port ${PORT} --strictPort`,
    url: `http://127.0.0.1:${PORT}/`,
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
    stdout: 'ignore',
    stderr: 'pipe',
  },
});
