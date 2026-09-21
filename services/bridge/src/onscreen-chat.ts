/**
 * Forwarding chat to the screen (`docs/stream-mvp-plan.md`, "Rail layout v2": a persistent CHAT
 * panel of the last ~7 lines, AutoMod-passed, service-sanitized, deny-listed, kill-switched).
 *
 * The whole path, and what each step is responsible for:
 *
 * ```
 * Twitch -> AutoMod -> flybridge (this module) -> POST /chat -> flysim (sanitize, deny list,
 *                                                                      ring) -> feed header -> page
 * ```
 *
 * **AutoMod comes first, and nothing here can substitute for it.** A message held by AutoMod never
 * arrives over EventSub at all: `channel.chat.message` fires for messages that were *delivered* to
 * chat, so a held message produces no event, and a message a moderator later deletes produces a
 * separate `channel.chat.message_delete` we do not subscribe to. Everything this module sees has
 * already passed the channel's moderation settings.
 *
 * What this module adds on top:
 *  - `validateDisplayName`, the same chokepoint viewer names already go through (`src/names.ts`);
 *  - `sanitizeChatText`, the same rules flysim enforces again on arrival (`@flybrain/feed`);
 *  - commands are not chatter: a line starting with `!` is a bot instruction and is dropped here,
 *    so `!sugar` does not appear on the broadcast twice (once as a command, once as a line);
 *  - known third-party bots are dropped, because a bot talking to a bot is not audience;
 *  - our own replies go to the screen from the wrapped sender (`wrapSendWithOnscreenEcho`) with
 *    `bot: true`, marked as the bot's, and the EventSub echo of that same message is dropped as a
 *    duplicate. A reply longer than the sanitizer's cap is shortened for the panel only
 *    ([`shortenForPanel`]); its echo is over the cap and so is refused anyway, which is the same
 *    outcome by a different door.
 *
 * Nothing here is authoritative: flysim re-runs every rule and applies the deny list and the rate
 * limits regardless of what this module does (`docs/control-api.md`). This is the cheap first pass,
 * so hostile text never even leaves the process.
 *
 * `FEATURE_ONSCREEN_CHAT=false` turns the forwarding off without touching flysim's own kill switch.
 */
import {
  CHAT_MAX_TEXT_LENGTH,
  FALLBACK_DISPLAY_NAME,
  sanitizeChatText,
  validateDisplayName,
} from '@flybrain/feed';
import type { BridgeConfig } from './config';
import type { Send } from './chat';
import type { Metrics } from './health';
import type { Clock } from './ratelimit';
import { systemClock } from './ratelimit';
import type { SimClient } from './sim';
import type { TemplateId, TemplateParams } from './templates';

/**
 * Third-party chat bots whose output is not audience chatter.
 *
 * Logins, lower-cased. Our own bot account is added at construction from `config.botUser`, so it
 * does not have to be listed here and the two demo channels can use different bot accounts.
 */
export const KNOWN_BOT_LOGINS: readonly string[] = [
  'nightbot',
  'streamelements',
  'streamlabs',
  'moobot',
  'fossabot',
  'wizebot',
  'botisimo',
  'phantombot',
  'sery_bot',
  'soundalerts',
  'own3d',
  'commanderroot',
  'streamlabs_chatbot',
];

/** How long a self-posted line is remembered, so its EventSub echo is recognised as a duplicate. */
export const ECHO_WINDOW_MS = 15_000;

/** What happened to one message. Everything but `sent` means nothing was posted to flysim. */
export type ForwardOutcome =
  | 'sent'
  | 'disabled'
  | 'command'
  | 'bot'
  | 'echo'
  | 'invalid_name'
  | 'rejected'
  | 'sim_refused'
  | 'sim_unreachable';

/** One incoming chat message, as much of it as this module is allowed to care about. */
export interface IncomingChatMessage {
  /** `chatterUserLogin` from EventSub, or the display name lower-cased if that is all there is. */
  login: string;
  /** `chatterDisplayName` from EventSub. */
  displayName: string;
  /** The message body. This is the only place raw chat text exists in this service. */
  messageText: string;
}

export interface OnscreenChatOptions {
  sim: SimClient;
  config: Pick<BridgeConfig, 'featureOnscreenChat' | 'botUser'>;
  metrics?: Pick<Metrics, 'increment'>;
  clock?: Clock;
  /** Extra bot logins to drop, on top of `KNOWN_BOT_LOGINS`. */
  extraBotLogins?: readonly string[];
}

/**
 * Forwards viewer messages and the bridge's own replies to `POST /chat`.
 *
 * Stateless apart from the echo memory, and twurple-free: `src/eventsub.ts` hands it a plain
 * `IncomingChatMessage`, so `tests/onscreen-chat.test.ts` drives the same code the stream does.
 */
export class OnscreenChat {
  private readonly sim: SimClient;
  private readonly enabled: boolean;
  private readonly botLogin: string;
  private readonly botLogins: Set<string>;
  private readonly metrics: Pick<Metrics, 'increment'> | undefined;
  private readonly clock: Clock;
  /** Sanitized text of lines this bridge posted itself, with the wall clock it posted them at. */
  private readonly recentSelfPosts = new Map<string, number>();

  constructor(options: OnscreenChatOptions) {
    this.sim = options.sim;
    this.enabled = options.config.featureOnscreenChat;
    this.botLogin = options.config.botUser.toLowerCase();
    this.botLogins = new Set([
      ...KNOWN_BOT_LOGINS,
      ...(options.extraBotLogins ?? []).map((login) => login.toLowerCase()),
    ]);
    this.metrics = options.metrics;
    this.clock = options.clock ?? systemClock;
  }

  /** Whether `FEATURE_ONSCREEN_CHAT` is on. flysim has its own, independent kill switch. */
  get isEnabled(): boolean {
    return this.enabled;
  }

  /**
   * Forward one viewer message, if every rule here allows it.
   *
   * Never throws: a sim that is down is a dropped line and a counter, not an exception into
   * twurple's event loop.
   */
  async forwardViewerMessage(message: IncomingChatMessage): Promise<ForwardOutcome> {
    if (!this.enabled) return this.count('disabled');

    const login = message.login.toLowerCase();
    // A command is an instruction to this bot, not something the audience said.
    if (isCommand(message.messageText)) return this.count('command');

    const ownMessage = login === this.botLogin;
    if (!ownMessage && this.botLogins.has(login)) return this.count('bot');

    const by = validateDisplayName(message.displayName);
    if (by === FALLBACK_DISPLAY_NAME && message.displayName !== FALLBACK_DISPLAY_NAME) {
      // flysim would refuse this name too; not worth a round trip.
      return this.count('invalid_name');
    }

    const text = sanitizeChatText(message.messageText);
    if (text === null) return this.count('rejected');

    // Our own replies reach the screen from `send()`, so the EventSub echo of one we already
    // posted is a duplicate. An echo we do NOT recognise (a post that failed, or a human using
    // the bot account) still goes up, marked as the bot's.
    if (ownMessage && this.consumeEcho(text)) return this.count('echo');

    return this.post(by, text, ownMessage);
  }

  /**
   * Put one of the bridge's own template replies on the screen, marked `bot: true`.
   *
   * Called by [`wrapSendWithOnscreenEcho`] after the message went to Twitch, so a line only
   * appears on the broadcast if chat actually got it.
   */
  async forwardBotReply(text: string): Promise<ForwardOutcome> {
    if (!this.enabled) return this.count('disabled');
    const sanitized = sanitizeChatText(shortenForPanel(text));
    if (sanitized === null) return this.count('rejected');
    this.rememberSelfPost(sanitized);
    return this.post(validateDisplayName(this.botLogin), sanitized, true);
  }

  private async post(by: string, text: string, bot: boolean): Promise<ForwardOutcome> {
    const result = await this.sim.chat({ by, text, bot });
    if (result.ok) return this.count('sent');
    // 403 (flysim's own kill switch), 422 (a rule refused it) and 429 (a rate limit) are all
    // "the service said no", and all normal. Anything else means the sim is not answering.
    if (result.kind === 'forbidden' || result.kind === 'rate_limited' || result.kind === 'http_error') {
      return this.count('sim_refused');
    }
    return this.count('sim_unreachable');
  }

  private rememberSelfPost(text: string): void {
    this.pruneEchoes();
    this.recentSelfPosts.set(text, this.clock.now());
  }

  /** Whether `text` is one this bridge posted recently; consumed, so a repeat is not swallowed. */
  private consumeEcho(text: string): boolean {
    this.pruneEchoes();
    if (!this.recentSelfPosts.has(text)) return false;
    this.recentSelfPosts.delete(text);
    return true;
  }

  private pruneEchoes(): void {
    const now = this.clock.now();
    for (const [text, at] of this.recentSelfPosts) {
      if (now - at > ECHO_WINDOW_MS) this.recentSelfPosts.delete(text);
    }
  }

  private count(outcome: ForwardOutcome): ForwardOutcome {
    this.metrics?.increment(`flybridge_onscreen_chat_total{outcome="${outcome}"}`);
    return outcome;
  }
}

/** Whether a message is a bot command rather than chatter. */
export function isCommand(messageText: string): boolean {
  return messageText.trimStart().startsWith('!');
}

/**
 * Shorten one of **our own** template replies to the sanitizer's cap, at a word boundary, with an
 * ellipsis.
 *
 * Several templates are longer than 200 characters — fine for Twitch chat, over the cap for the
 * on-screen panel — and a reply that vanished from the panel would be a silent gap next to the
 * viewer line it was answering. This only ever touches text this service wrote itself
 * (`renderTemplate`, `src/templates.ts`); a viewer's line over the cap is refused whole, never
 * trimmed, because editing what someone said is worse than dropping it.
 *
 * The Twitch message is unaffected: it is sent in full before this runs.
 */
export function shortenForPanel(text: string, limit = CHAT_MAX_TEXT_LENGTH): string {
  const characters = [...text];
  if (characters.length <= limit) return text;
  const head = characters.slice(0, limit - 1).join('');
  const lastSpace = head.lastIndexOf(' ');
  const cut = lastSpace > limit / 2 ? head.slice(0, lastSpace) : head;
  return `${cut.trimEnd()}…`;
}

/**
 * Wrap a `send()` so every template reply that reaches Twitch also reaches the screen with
 * `bot: true`.
 *
 * The wrapper keeps `send`'s exact signature, so the template lint in `tests/templates.test.ts`
 * still sees `send('<templateId>', ...)` call sites and the only way to produce an outbound string
 * is still `renderTemplate` (`src/templates.ts`).
 */
export function wrapSendWithOnscreenEcho(send: Send, onscreen: OnscreenChat, renderer: Renderer): Send {
  return async function sendAndEcho<T extends TemplateId>(id: T, params: TemplateParams<T>): Promise<void> {
    await send(id, params);
    await onscreen.forwardBotReply(renderer(id, params));
  };
}

/** `renderTemplate`, as a type the wrapper can take without importing the whole module. */
export type Renderer = <T extends TemplateId>(id: T, params: TemplateParams<T>) => string;
