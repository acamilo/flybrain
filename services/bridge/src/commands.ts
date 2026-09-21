/**
 * The five B1 chat commands (`docs/design/stage-bridge.md` B2): `!fly`, `!brain`, `!how`,
 * `!stuck`, `!sugar`.
 *
 * Pure dispatch logic, independent of twurple — `eventsub.ts` parses an incoming chat message,
 * calls `parseCommand`, and hands the result to `handleCommand` with a fake-free `SimClient` and
 * `Send`. This is what `tests/commands.test.ts` exercises directly with a fake chat sender and
 * the fake sim from `@flybrain/feed/fake`.
 */
import type { BridgeConfig } from './config';
import { formatDuration } from './duration';
import { validateDisplayName } from './names';
import type { RateLimiter } from './ratelimit';
import type { SimClient } from './sim';
import type { Send } from './chat';

export type CommandName = 'fly' | 'brain' | 'how' | 'stuck' | 'sugar';

export const COMMAND_NAMES: readonly CommandName[] = ['fly', 'brain', 'how', 'stuck', 'sugar'];

const COMMAND_TRIGGERS: Record<string, CommandName> = {
  '!fly': 'fly',
  '!brain': 'brain',
  '!how': 'how',
  '!stuck': 'stuck',
  '!sugar': 'sugar',
};

/** Parse the first whitespace-delimited token of a chat message as a command, or `null`. */
export function parseCommand(messageText: string): CommandName | null {
  const trigger = messageText.trim().split(/\s+/, 1)[0]?.toLowerCase();
  if (!trigger) return null;
  return COMMAND_TRIGGERS[trigger] ?? null;
}

export interface CommandUser {
  id: string;
  displayName: string;
}

export interface CommandDeps {
  sim: SimClient;
  send: Send;
  rateLimiter: RateLimiter;
  config: Pick<BridgeConfig, 'gameTitle'>;
}

/**
 * Handle one parsed command for one user. Rate-limited commands are dropped silently (standard
 * anti-spam practice); `!sugar`'s outcome templates instead report what the sim actually said,
 * per B2: "additionally consults the sim's own cooldown so the answer is truthful."
 */
export async function handleCommand(command: CommandName, user: CommandUser, deps: CommandDeps): Promise<void> {
  const decision = deps.rateLimiter.tryConsume(user.id, command);
  if (!decision.allowed) return;

  switch (command) {
    case 'fly':
      await deps.send('fly', { gameTitle: deps.config.gameTitle });
      return;
    case 'brain':
      await deps.send('brain', { gameTitle: deps.config.gameTitle });
      return;
    case 'how':
      await deps.send('how', { gameTitle: deps.config.gameTitle });
      return;
    case 'stuck':
      await handleStuck(deps);
      return;
    case 'sugar':
      await handleSugar(user, deps);
      return;
  }
}

async function handleStuck(deps: CommandDeps): Promise<void> {
  const result = await deps.sim.status();
  if (!result.ok) return; // sim unreachable; surfaced via /health rather than chat noise
  const { milestone } = result.data;
  await deps.send('stuck', { label: milestone.label, durationLabel: formatDuration(milestone.sinceSeconds) });
}

async function handleSugar(user: CommandUser, deps: CommandDeps): Promise<void> {
  const by = validateDisplayName(user.displayName);
  const result = await deps.sim.stimulate({ by, source: 'chat' });

  if (result.ok) {
    await deps.send('sugarAccepted', { by });
    return;
  }

  if (result.kind === 'rate_limited') {
    await deps.send('sugarCooldown', { retryAfterSeconds: Math.max(1, Math.ceil(result.retryAfterMs / 1000)) });
    return;
  }

  // forbidden / timeout / http_error / network_error: sugar isn't working right now.
  await deps.send('sugarDisabled', {});
}
