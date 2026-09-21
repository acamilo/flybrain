/**
 * Predictions stub (`docs/design/stage-bridge.md` B3, item 5). **Disabled in B1.**
 *
 * Predictions require Twitch Affiliate status (`docs/stream-mvp-plan.md`: "Polls, Predictions,
 * Hype Train still Affiliate-only"), which neither demo channel has yet, and the
 * `channel:manage:predictions` scope, which is only asserted when `FEATURE_PREDICTIONS` is on
 * (`src/scopes.ts`). There is no real Twitch channel to test this against in B1, so this module
 * is intentionally a stub: it exists so `src/index.ts` has a stable seam to wire up later
 * ("open on milestone start, resolve on rank change or timeout"), and so the feature flag has
 * something concrete to gate.
 *
 * When this is implemented: open a prediction on milestone start ("will the fly reach X within
 * 4 hours?"), resolve it on rank change or timeout, using `channel.prediction.*` EventSub events
 * and `createPrediction`/`endPrediction` from `@twurple/api`.
 */

export interface PredictionsManager {
  /** No-op in B1. Real implementation opens/resolves predictions on milestone events. */
  start(): Promise<void>;
  stop(): void;
}

export function createDisabledPredictionsManager(): PredictionsManager {
  return {
    async start(): Promise<void> {
      // Intentionally does nothing: FEATURE_PREDICTIONS is off in B1 (see module docstring).
    },
    stop(): void {
      // Nothing to stop.
    },
  };
}
