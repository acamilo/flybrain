import { useLayoutEffect, useRef } from 'react';

import { useStage } from '@/feed/store';
import { LAYOUT } from '@/lib/geometry';
import { DATASET_COPY } from '@/lib/labels';
import type { StageChat } from '@/lib/query';
import { Panel } from './Panel';

/**
 * Rail row 4: the persistent chat panel (locked layout v2).
 *
 * The last seven lines of `header.chat`, which the service has already validated and sanitized
 * (`docs/feed-protocol.md`: "<= 200 chars, printable Unicode letters/digits/punctuation/space
 * only, no control chars, no URLs, deny-list filtered; the service refuses anything else"), and
 * which `src/chat/sanitize.ts` puts through the same rules again on the way to this panel.
 *
 * Strictly text. No links, no images, no markup, no embeds: the lines arrive as strings, they are
 * rendered as strings, and a line that fails re-validation is dropped rather than repaired.
 *
 * **Absent means absent.** With no `chat` in the header — an older service, or the `?chat=off`
 * kill switch — this renders `null`: no panel, no border, no title, no empty box that looks like
 * a broken chat. That is the case the structural test pins, because "the panel is there but
 * empty" is exactly what a viewer reads as "the stream is broken".
 *
 * Bot lines green, names amber, text ink (the design's colour split): a viewer has to be able to
 * tell the bridge's own template replies from a person at a glance, because the bridge is the only
 * thing on this stream that can be made to say something by accident. The name also carries the
 * label face rather than the text one (the operator, 2026-09-17: "make name pop more"), which is the whole
 * of the distinction at the 0.31 downscale — Silkscreen's boxy stems survive it where VT323's
 * hairlines are what the ellipsis used to eat.
 *
 * ## Wrapping, and why it needs a measurement
 *
 * the operator, 2026-09-17: "make chat messages wrap." A line is up to 200 characters, so it takes as many
 * visual rows as it needs and the panel keeps the most recent lines that *fit* — the oldest fall
 * off the top, which is what chat does everywhere. The panel's height is fixed (244 px, the locked
 * layout), so "what fits" is a fact about the rendered rows and not something CSS can express:
 * `justify-content: flex-end` on an overflowing column puts the newest line at the bottom and
 * clips the overflow at the top, but it clips it *mid-glyph*, and half a line of someone's text is
 * the one thing worse than no line at all.
 *
 * So the rows are measured after layout and the ones that do not fit whole are marked
 * `data-fits="0"`, which `src/theme/rail.css` draws as `visibility: hidden` — hidden, not removed,
 * because a removed row would change the layout the next measurement reads and the two would
 * chase each other forever. The newest row is always kept, even in the pathological case where one
 * line is taller than the panel: the alternative is a chat panel showing nothing.
 */
export function ChatPanel({ source }: { source: StageChat }) {
  const lines = useStage((state) => state.chat);
  const box = useRef<HTMLDivElement>(null);

  /**
   * Which rows fit, bottom up.
   *
   * Keyed on the ids and the text lengths rather than on `lines`: the store commits a fresh array
   * four times a second whether or not the ring moved, and this forces a synchronous layout.
   */
  const shape = lines.map((line) => `${line.id}:${line.text.length}`).join('|');
  useLayoutEffect(() => {
    const element = box.current;
    if (!element) return;
    const rows = [...element.children] as HTMLElement[];
    const gap = Number.parseFloat(getComputedStyle(element).rowGap) || 0;
    const budget = element.clientHeight;
    let used = 0;
    for (let index = rows.length - 1; index >= 0; index -= 1) {
      const row = rows[index] as HTMLElement;
      used += row.offsetHeight + (index === rows.length - 1 ? 0 : gap);
      row.dataset.fits = used <= budget || index === rows.length - 1 ? '1' : '0';
    }
  }, [shape]);

  if (source === 'off' || lines.length === 0) return null;

  return (
    <Panel box={LAYOUT.chat} bodyClassName="chat" testId="chat-panel" critical>
      <span className="panel-title">{DATASET_COPY.chatTitle}</span>
      <div className="chat__lines" data-testid="chat-lines" ref={box}>
        {lines.map((line) => (
          <div
            className="chat-line"
            key={line.id}
            data-chat-id={line.id}
            data-bot={line.bot ? '1' : '0'}
            /* Written by the layout effect above. Declared here so a row is never briefly
               invisible on its first frame, which on a broadcast is a flicker. */
            data-fits="1"
          >
            <span data-role="body" className="chat-line__name">
              {line.by}
            </span>{' '}
            <span data-role="body" className="chat-line__text">
              {line.text}
            </span>
          </div>
        ))}
      </div>
    </Panel>
  );
}
