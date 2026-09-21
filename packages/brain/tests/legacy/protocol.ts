// Verbatim button constants from fly-plays-pokemon src/protocol.ts (oracle reference; do not edit).
export const BUTTONS = ['up', 'down', 'left', 'right', 'a', 'b', 'start', 'select'] as const;
export type Button = (typeof BUTTONS)[number];
export const BUTTON_BITS: Record<Button, number> = {
  up: 1 << 0, down: 1 << 1, left: 1 << 2, right: 1 << 3, a: 1 << 4, b: 1 << 5, start: 1 << 6, select: 1 << 7,
};
