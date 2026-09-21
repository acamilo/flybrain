/**
 * Game config registry, selected by `?game=`.
 *
 * Static imports, not dynamic: two configs are a few kilobytes, the page must be able to paint
 * before any network round-trip finishes, and a broken `?game=` must fall back rather than throw
 * on a live broadcast.
 */
import { platformer } from './platformer';
import { pokemonRed } from './pokemon-red';
import type { GameConfig } from './types';

export type { GameConfig, GameCounter, RewardCopy, RewardTier } from './types';

export const GAMES: Record<string, GameConfig> = {
  'pokemon-red': pokemonRed,
  platformer,
};

export const DEFAULT_GAME = 'pokemon-red';

/** Resolve a `?game=` value, falling back to the default rather than failing on air. */
export function resolveGame(id: string | null | undefined): GameConfig {
  const config = id ? GAMES[id] : undefined;
  return config ?? (GAMES[DEFAULT_GAME] as GameConfig);
}
