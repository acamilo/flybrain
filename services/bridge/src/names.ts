/**
 * Re-export of the single display-name validation chokepoint.
 *
 * The real implementation lives in `packages/feed/src/names.ts` (`@flybrain/feed`) so that
 * flystage and flybridge both validate viewer names the same way — see
 * `docs/design/stage-bridge.md` section C, "How viewer names reach the ticker". This module
 * exists only so bridge code can `import { validateDisplayName } from './names'` like every
 * other local module, without every call site reaching across the workspace boundary by hand.
 */
export { FALLBACK_DISPLAY_NAME, validateDisplayName } from '@flybrain/feed';
