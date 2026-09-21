/**
 * Rotating explainer poster (`docs/design/stage-bridge.md` B2): every `explainerIntervalMs`
 * (default 20 min) post the next of six explainer cards, unless chat has been silent for
 * `explainerSilenceMs` (default 20 min) — "so it does not shout into an empty room."
 *
 * Driven by an injected `Clock` and an explicit `tick()` rather than real timers, so
 * `tests/explainer.test.ts` can exercise the silence rule deterministically. `src/index.ts`
 * calls `tick()` on a real interval and `noteChatActivity()` on every incoming chat message.
 */
import type { BridgeConfig } from './config';
import type { Send } from './chat';
import type { Clock } from './ratelimit';
import { systemClock } from './ratelimit';
import { EXPLAINER_TEMPLATE_IDS, type TemplateId } from './templates';

export interface ExplainerPosterOptions {
  send: Send;
  config: Pick<BridgeConfig, 'gameTitle'>;
  intervalMs: number;
  silenceMs: number;
  clock?: Clock;
  /**
   * Whether the rotation runs at all. Defaults to `intervalMs > 0`, so an interval of zero is
   * "off" without a caller having to say so twice; `src/index.ts` passes `explainerEnabled(config)`
   * so `FEATURE_QUIET` turns it off too. A disabled poster still accepts `noteChatActivity()` —
   * the call site is the chat handler, which should not have to know.
   */
  enabled?: boolean;
}

/**
 * Whether this deployment runs the explainer rotation at all.
 *
 * Two independent switches, per the operator 2026-09-17 ("make bot less chatty. speaks only when spoken
 * to"): `FEATURE_QUIET` silences everything self-initiated, and `EXPLAINER_INTERVAL_MS=0` turns
 * off just this rotation on a channel that still greets and posts notices.
 */
export function explainerEnabled(config: Pick<BridgeConfig, 'featureQuiet' | 'explainerIntervalMs'>): boolean {
  return !config.featureQuiet && config.explainerIntervalMs > 0;
}

export class ExplainerPoster {
  private readonly send: Send;
  private readonly config: Pick<BridgeConfig, 'gameTitle'>;
  private readonly intervalMs: number;
  private readonly silenceMs: number;
  private readonly clock: Clock;
  private readonly enabled: boolean;

  private cardIndex = 0;
  private lastPostAt: number;
  private lastChatActivityAt: number;

  constructor(options: ExplainerPosterOptions) {
    this.send = options.send;
    this.config = options.config;
    this.intervalMs = options.intervalMs;
    this.silenceMs = options.silenceMs;
    this.clock = options.clock ?? systemClock;
    this.enabled = options.enabled ?? options.intervalMs > 0;
    const now = this.clock.now();
    this.lastPostAt = now;
    this.lastChatActivityAt = now;
  }

  /** Call on every incoming chat message (any message, not just commands) to reset the silence clock. */
  noteChatActivity(): void {
    this.lastChatActivityAt = this.clock.now();
  }

  /**
   * Call periodically. Posts the next rotation card and returns `true` if `intervalMs` has
   * elapsed since the last post attempt and chat has not been silent for `silenceMs`; otherwise
   * returns `false`. The interval always resets on a due check, silent or not, so a long quiet
   * stretch does not burst-post once chat picks back up. A disabled poster (quiet mode, or
   * `EXPLAINER_INTERVAL_MS=0`) always returns `false` and never sends.
   */
  async tick(): Promise<boolean> {
    if (!this.enabled) return false;
    const now = this.clock.now();
    if (now - this.lastPostAt < this.intervalMs) return false;
    this.lastPostAt = now;

    if (now - this.lastChatActivityAt >= this.silenceMs) return false;

    const id = EXPLAINER_TEMPLATE_IDS[this.cardIndex % EXPLAINER_TEMPLATE_IDS.length]!;
    this.cardIndex += 1;
    await postExplainerCard(this.send, id, this.config);
    return true;
  }
}

async function postExplainerCard(send: Send, id: TemplateId, config: Pick<BridgeConfig, 'gameTitle'>): Promise<void> {
  switch (id) {
    case 'explainerConnectome':
      await send('explainerConnectome', {});
      return;
    case 'explainerButtons':
      await send('explainerButtons', { gameTitle: config.gameTitle });
      return;
    case 'explainerReward':
      await send('explainerReward', { gameTitle: config.gameTitle });
      return;
    case 'explainerSugar':
      await send('explainerSugar', {});
      return;
    case 'explainerHonesty':
      await send('explainerHonesty', {});
      return;
    case 'explainerRepo':
      await send('explainerRepo', {});
      return;
    default:
      return; // unreachable: EXPLAINER_TEMPLATE_IDS only ever contains the cases above
  }
}
