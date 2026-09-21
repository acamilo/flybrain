/**
 * Short names for the macro pad's cells.
 *
 * The pad under the game is two columns of seven at the 24 px body floor
 * (`docs/design/macros.md` section 14), which leaves a cell 179 px: 85 for the channel tag's chip
 * and 87 for the name, nine characters of VT323 (`src/lib/geometry.ts`). Six of the contract's
 * names are longer than that, so the pad draws a short form and the MACROS tab — whose cells are
 * 332 px — draws the contract's own name in full. Nothing here is a second vocabulary: a short
 * name is the same words, shortened the way the channel tag already shortens them
 * (`GO OBJECTIVE` / `MB·GOAL` -> `GO GOAL`), so a viewer reading the tab and then the pad sees one
 * button and not two.
 *
 * **Why a table and not a rule.** A rule that cut words to fit produced `GO OBJEC` and
 * `BUY ANTID`, which read as a rendering bug at the floor rather than as an abbreviation. The
 * table is six lines; the rule below it is the fallback, and it exists for exactly one case: a
 * producer or a contract ahead of this page. `tests/unit/macro-names.test.ts` holds every type in
 * `MACRO_TYPES` to the cap and to being distinct, so a name added to the contract that needs a
 * line here fails the suite rather than the broadcast.
 */
import { MACRO_SHORT_NAME_MAX } from './geometry';

/**
 * The names that do not fit, shortened.
 *
 * Keyed on the contract's name, and deliberately including the nine types of
 * `docs/design/macros.md` sections 13 and 14 that are still in flight: a table that only knows
 * today's contract would put `BUY ANTID` on air on the day they land.
 */
const SHORT_NAMES: Record<string, string> = {
  'GO OBJECTIVE': 'GO GOAL',
  'GO FRONTIER': 'GO FRONT',
  'BUY POTION': 'BUY POTN',
  'BUY ANTIDOTE': 'BUY ANTI',
  // The fly's own ball, against the mart's `BUY BALL`: the verb is what distinguishes them and the
  // noun is what does not fit, so the verb is what stays.
  'THROW BALL': 'THROW',
};

/**
 * The name the pad's cell draws for a macro type.
 *
 * Total, and never empty: a name that fits is returned unchanged, a name in the table is its short
 * form, and anything else is cut to the cap at a word boundary if there is one inside it. No
 * ellipsis — a character of "…" is a character of name at this size, and the cell's `overflow`
 * remains the structural backstop.
 */
export function shortMacroName(name: string): string {
  if (name.length <= MACRO_SHORT_NAME_MAX) return name;

  const short = SHORT_NAMES[name];
  if (short !== undefined) return short;

  const space = name.lastIndexOf(' ', MACRO_SHORT_NAME_MAX);
  return space > 0 ? name.slice(0, space) : name.slice(0, MACRO_SHORT_NAME_MAX);
}
