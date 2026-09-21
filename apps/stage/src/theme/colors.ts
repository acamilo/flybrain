/**
 * Read theme colours out of the CSS variables, so the canvases and the DOM agree.
 *
 * The panels are styled from `--sensory`, `--motor` and friends; the three canvases paint pixels
 * and need numbers. Resolving them from `getComputedStyle` at startup means a theme swap changes
 * both, and there is still exactly one definition of each colour (in `tokens.css`).
 */

export type Rgb = [number, number, number];

/** Parse `#rgb`, `#rrggbb` or `rgb(r g b)` / `rgb(r, g, b)`. Returns `fallback` on anything else. */
export function parseColor(value: string, fallback: Rgb): Rgb {
  const text = value.trim();
  if (text.startsWith('#')) {
    const hex = text.slice(1);
    if (hex.length === 3) {
      const r = hex[0] as string;
      const g = hex[1] as string;
      const b = hex[2] as string;
      return [Number.parseInt(r + r, 16), Number.parseInt(g + g, 16), Number.parseInt(b + b, 16)];
    }
    if (hex.length === 6) {
      return [
        Number.parseInt(hex.slice(0, 2), 16),
        Number.parseInt(hex.slice(2, 4), 16),
        Number.parseInt(hex.slice(4, 6), 16),
      ];
    }
    return fallback;
  }

  const match = /rgba?\(([^)]+)\)/i.exec(text);
  if (!match) return fallback;
  const parts = (match[1] as string)
    .split(/[\s,/]+/)
    .filter(Boolean)
    .map((part) => Number.parseFloat(part));
  if (parts.length < 3 || parts.some((part) => !Number.isFinite(part))) return fallback;
  return [parts[0] as number, parts[1] as number, parts[2] as number];
}

/** Resolve one CSS custom property on an element to RGB. */
export function cssColor(element: Element, property: string, fallback: Rgb): Rgb {
  const value = getComputedStyle(element).getPropertyValue(property);
  return value ? parseColor(value, fallback) : fallback;
}

/** Every colour the canvases need, resolved once. */
export interface CanvasPalette {
  sensory: Rgb;
  internal: Rgb;
  output: Rgb;
  dopamine: Rgb;
  background: Rgb;
  panel: Rgb;
}

export function readCanvasPalette(root: Element = document.documentElement): CanvasPalette {
  return {
    sensory: cssColor(root, '--sensory', [79, 195, 247]),
    // No `--internal` token: the point cloud's bulk is deliberately the quiet ink colour.
    internal: cssColor(root, '--ink-2', [127, 138, 156]),
    output: cssColor(root, '--accent', [255, 176, 32]),
    dopamine: cssColor(root, '--dopamine', [244, 114, 168]),
    background: cssColor(root, '--bg-0', [8, 9, 13]),
    panel: cssColor(root, '--panel', [20, 24, 34]),
  };
}
