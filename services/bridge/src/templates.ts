/**
 * Every outbound chat string, as a constant with typed placeholders.
 *
 * This is the "Nothing, Forever" lesson (`docs/stream-mvp-plan.md`) encoded as a type: chat
 * replies are template lookups, never string concatenation of chat input, never a model. A call
 * site can only ever pass a `TemplateId` (a string literal from `TEMPLATE_IDS`) plus a params
 * object shaped for that specific id — see `renderTemplate` below and `src/chat.ts`'s `send()`,
 * which is the only function allowed to put a rendered template on the wire. Game-specific words
 * (the title, the channel) are never hardcoded here: they arrive as params sourced from
 * `BridgeConfig` (`src/config.ts`), so the same code serves both demos.
 *
 * `tests/templates.test.ts` includes a lint test asserting every `send(...)` call site in `src/`
 * passes a string literal from `TEMPLATE_IDS`, never a variable or template literal.
 */

/** Every template id. Order here is also the source of truth `TEMPLATE_IDS` is generated from. */
export type TemplateId =
  | 'startup'
  | 'recovered'
  | 'fly'
  | 'brain'
  | 'how'
  | 'stuck'
  | 'sugarAccepted'
  | 'sugarCooldown'
  | 'sugarDisabled'
  | 'followThanks'
  | 'raidThanks'
  | 'explainerConnectome'
  | 'explainerButtons'
  | 'explainerReward'
  | 'explainerSugar'
  | 'explainerHonesty'
  | 'explainerRepo';

interface TemplateParamsMap {
  startup: { channel: string; gameTitle: string };
  recovered: { channel: string; gameTitle: string };
  fly: { gameTitle: string };
  brain: { gameTitle: string };
  how: { gameTitle: string };
  stuck: { label: string; durationLabel: string };
  sugarAccepted: { by: string };
  sugarCooldown: { retryAfterSeconds: number };
  sugarDisabled: Record<string, never>;
  followThanks: { by: string };
  raidThanks: { by: string; viewers: number };
  explainerConnectome: Record<string, never>;
  explainerButtons: { gameTitle: string };
  explainerReward: { gameTitle: string };
  explainerSugar: Record<string, never>;
  explainerHonesty: Record<string, never>;
  explainerRepo: Record<string, never>;
}

/** The params shape a given `TemplateId` requires. */
export type TemplateParams<T extends TemplateId> = TemplateParamsMap[T];

type TemplateRenderers = { [K in TemplateId]: (params: TemplateParamsMap[K]) => string };

const TEMPLATES: TemplateRenderers = {
  startup: ({ channel, gameTitle }) =>
    `flybridge is online for #${channel}. A simulated fruit-fly brain is playing ${gameTitle} live. Say !how to learn what's real.`,

  // Posted instead of `startup` when the previous process exited itself to recover a dead chat
  // subscription (`src/subscription-health.ts`, `src/notice.ts`). Says what a viewer actually
  // needs — the chat bot went away, the fly did not — and `NoticeLog` guarantees at most one
  // notice of either kind per ten minutes, so a bad half hour on Twitch's side cannot spam it.
  recovered: ({ channel, gameTitle }) =>
    `flybridge is back in #${channel}. Its Twitch chat connection dropped and it reconnected. The fly never stopped playing ${gameTitle}.`,

  fly: ({ gameTitle }) =>
    `A simulated fly brain (139,255 neurons, FlyWire connectome) is playing ${gameTitle}. Its motor neurons pick from a short menu of actions for the current scene; the sim presses the buttons for the one it picked. Nobody else presses anything.`,

  brain: ({ gameTitle }) =>
    `Spiking network from a real fly's mapped brain, playing ${gameTitle}. It sees the screen. Rewards from the game nudge a few thousand synapses, so what worked gets a little likelier. In battles the scene weighs in more; out in the world it's mostly the fly.`,

  how: ({ gameTitle }) =>
    `!fly what this is · !brain how it works · !stuck time on the current milestone · !sugar a small reward pulse (no buttons). The fly plays ${gameTitle} on its own; the menu under the screen is what it can pick from right now.`,

  stuck: ({ label, durationLabel }) => `Current milestone: "${label}", ${durationLabel} and counting. The fly will get there.`,

  sugarAccepted: ({ by }) => `Sugar from ${by}! The fly gets a brief PAM reward pulse. It doesn't press any buttons, just feels good for a moment.`,

  sugarCooldown: ({ retryAfterSeconds }) => `Sugar is on cooldown. Try again in ${retryAfterSeconds}s.`,

  sugarDisabled: () => `Sugar is turned off right now.`,

  followThanks: ({ by }) => `Thanks for the follow, ${by}!`,

  raidThanks: ({ by, viewers }) => `Thanks for the raid, ${by}, and welcome to the ${viewers} of you joining!`,

  explainerConnectome: () =>
    `What you're watching: a spiking network wired from a real fly's mapped brain (FlyWire), running live. Not a script.`,

  explainerButtons: ({ gameTitle }) =>
    `The menu under the screen lists what the fly can do in this ${gameTitle} scene. Its motor neurons pick one; the sim presses the buttons for it. Chat has no path to the controller.`,

  explainerReward: () => `Game rewards nudge a few thousand of the fly's synapses toward what worked before. Small, slow, real.`,

  explainerSugar: () => `!sugar sends a tiny, timed dopamine pulse. Capped, rate-limited, never a button.`,

  explainerHonesty: () => `Scenes are read from the game's memory, never written. The plan for each scene is ours; the choice is the fly's, weighted by scene. All documented in the public repo.`,

  explainerRepo: () => `Curious how this works under the hood? The code, the model and the docs are all public. Ask in chat for the link.`,
};

/** Every valid template id, in declaration order. Used by the lint test and by `src/explainer.ts`. */
export const TEMPLATE_IDS: readonly TemplateId[] = Object.keys(TEMPLATES) as TemplateId[];

/** The six explainer rotation cards (`docs/design/stage-bridge.md` B2), in rotation order. */
export const EXPLAINER_TEMPLATE_IDS: readonly TemplateId[] = [
  'explainerConnectome',
  'explainerButtons',
  'explainerReward',
  'explainerSugar',
  'explainerHonesty',
  'explainerRepo',
];

export function isTemplateId(value: string): value is TemplateId {
  return (TEMPLATE_IDS as readonly string[]).includes(value);
}

/** Render a template. The only way to produce an outbound chat string. */
export function renderTemplate<T extends TemplateId>(id: T, params: TemplateParams<T>): string {
  const renderer = TEMPLATES[id] as (p: TemplateParams<T>) => string;
  return renderer(params);
}
