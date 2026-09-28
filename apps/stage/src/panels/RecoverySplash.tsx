import { useEffect, useState } from 'react';
import { create } from 'zustand';

import { LAYOUT, boxStyle } from '@/lib/geometry';
import { recoveryView, type RecoveryNotice, type RecoveryView } from '@/lib/recovery';

/**
 * The auto-recovery splash, over the game panel.
 *
 * `infra/bin/fly-loop-recover` announces a recovery a minute before it acts, then restarts flysim
 * or resets the run to an earlier rung; without this, viewers see the game freeze and jump for no
 * reason. The model is `src/lib/recovery.ts`, the data path `src/lib/recovery-poll.ts`.
 *
 * Two layouts, both in the Game Boy's own four greens, so it reads as the cartridge's text box
 * rather than as one more rail panel:
 *
 * - **box** (countdown, done, failed): a Pokémon-style text box across the bottom of the game,
 *   with the countdown beside it. The top two thirds of the game stay visible, because the stuck
 *   loop is the thing the countdown is about.
 * - **cover** (acting): the whole game panel, because the picture there is frozen or blank while
 *   flysim restarts.
 *
 * It re-renders at 4 Hz while a notice is held — the stage's own React cadence — and not at all
 * otherwise. The countdown is wall-clock arithmetic against the helper's `executeAt`, which is why
 * the page needs nothing but the file: it is right after a reload, and while the feed is down.
 */
export const useRecovery = create<{ notice: RecoveryNotice | null; pinnedNowS: number | null }>(() => ({
  notice: null,
  pinnedNowS: null,
}));

const TICK_MS = 250;

export function nowSeconds(pinned: number | null): number {
  return pinned ?? Date.now() / 1000;
}

export function RecoverySplash() {
  const notice = useRecovery((state) => state.notice);
  const pinned = useRecovery((state) => state.pinnedNowS);
  const [nowS, setNowS] = useState(() => nowSeconds(pinned));

  useEffect(() => {
    setNowS(nowSeconds(pinned));
    if (notice === null || pinned !== null) return;
    const timer = setInterval(() => setNowS(nowSeconds(null)), TICK_MS);
    return () => clearInterval(timer);
  }, [notice, pinned]);

  let view: RecoveryView | null = null;
  try {
    view = recoveryView(notice, nowS);
  } catch {
    view = null;
  }
  if (view === null) return null;

  return (
    <div
      className="recovery"
      data-testid="recovery-splash"
      data-phase={view.phase}
      data-action={view.action}
      data-layout={view.layout}
      style={{ ...boxStyle(LAYOUT.game), zIndex: 50 }}
    >
      {view.layout === 'cover' ? <Cover view={view} /> : <Box view={view} />}
    </div>
  );
}

function LoopLine({ view }: { view: RecoveryView }) {
  if (view.loop === '' && view.stuck === '') return null;
  return (
    <div className="recovery__loop" data-testid="recovery-loop">
      {view.loop !== '' ? <span className="recovery__loop-macros">{view.loop}</span> : null}
      {view.loop !== '' && view.stuck !== '' ? <span className="recovery__sep" aria-hidden /> : null}
      {view.stuck !== '' ? <span>{view.stuck}</span> : null}
    </div>
  );
}

function Box({ view }: { view: RecoveryView }) {
  return (
    <div className="recovery__box">
      <span className="recovery__chip">{view.chip}</span>
      <div className="recovery__row">
        <div className="recovery__text">
          <p className="recovery__headline" data-testid="recovery-headline">
            {view.headline}
          </p>
          <p className="recovery__body" data-testid="recovery-body">
            {view.body}
            {view.busy ? <span className="recovery__dots" aria-hidden /> : null}
          </p>
        </div>
        {view.countdown !== '' ? (
          <div className="recovery__countdown" data-testid="recovery-countdown">
            {view.countdown}
          </div>
        ) : null}
      </div>
      <LoopLine view={view} />
      <span className="recovery__arrow" aria-hidden />
    </div>
  );
}

function Cover({ view }: { view: RecoveryView }) {
  return (
    <div className="recovery__cover">
      <span className="recovery__chip recovery__chip--cover">{view.chip}</span>
      <p className="recovery__big" data-testid="recovery-headline">
        {view.headline}
        <span className="recovery__dots" aria-hidden />
      </p>
      <p className="recovery__body recovery__body--cover" data-testid="recovery-body">
        {view.body}
      </p>
      <div className={`recovery__bar recovery__bar--${view.action}`} aria-hidden>
        {Array.from({ length: 8 }, (_, index) => (
          <span key={index} style={{ animationDelay: `${index * 150}ms` }} />
        ))}
      </div>
      <div className="recovery__was">{view.loop !== '' ? 'IT WAS LOOPING ON' : null}</div>
      <LoopLine view={view} />
    </div>
  );
}
