/**
 * Chat sending: the `ChatSender` interface (so tests never import twurple), the template-only
 * `send()` gate, and the real Twurple-backed implementation.
 *
 * Each bridge deployment targets exactly one channel (`BridgeConfig.channel`), so `ChatSender`
 * takes only the message text — there is nothing else to route to.
 */
import type { ApiClient } from '@twurple/api';
import { renderTemplate, type TemplateId, type TemplateParams } from './templates';

export interface ChatSender {
  sendMessage(text: string): Promise<void>;
}

/** The only function allowed to put a rendered template on the wire. See `src/templates.ts`. */
export type Send = <T extends TemplateId>(id: T, params: TemplateParams<T>) => Promise<void>;

/** Build a `send()` bound to a `ChatSender`. Every command/event handler gets one of these. */
export function createSend(sender: ChatSender): Send {
  return async function send<T extends TemplateId>(id: T, params: TemplateParams<T>): Promise<void> {
    const text = renderTemplate(id, params);
    await sender.sendMessage(text);
  };
}

/**
 * Sends chat messages as the bot account, in the broadcaster's channel, using Twurple's
 * `asUser` context override (`docs/design/stage-bridge.md` B1: the bot token carries
 * `user:write:chat` + `user:bot`, the broadcaster authorizes it with `channel:bot`).
 */
export class TwurpleChatSender implements ChatSender {
  constructor(
    private readonly apiClient: ApiClient,
    private readonly botUserId: string,
    private readonly broadcasterUserId: string,
  ) {}

  async sendMessage(text: string): Promise<void> {
    await this.apiClient.asUser(this.botUserId, async (ctx) => {
      await ctx.chat.sendChatMessage(this.broadcasterUserId, text);
    });
  }
}
