import type { ReactNode } from 'react';

import { Card } from '@/components/ui/card';
import { boxStyle, type Box } from '@/lib/geometry';
import { cn } from '@/lib/utils';

/**
 * One absolutely-positioned panel: a shadcn Card at an exact box from `geometry.ts`, drawn as a
 * Game Boy dialogue box — square corners, a 4 px outer frame with a 2 px inner line, no shadow
 * (`docs/design/gameboy-theme.md`). The frame is all in CSS (`.panel` in `src/index.css`), so
 * there is nothing here to opt in or out of.
 *
 * Panels are placed, not flowed. The outer boxes are the verified arithmetic of design A2 and the
 * interiors are flex, so a panel's contents can never push it out of its box — which is the whole
 * class of bug behind the audit's wrapped footer and its everywhere-mobile type scale.
 */
export interface PanelProps {
  box: Box;
  /** Uppercased panel title, in the title face. Omit for a panel that carries its own header. */
  title?: string;
  /** Right-hand side of the title row, e.g. a badge. */
  aside?: ReactNode;
  className?: string;
  bodyClassName?: string;
  children?: ReactNode;
  /** Marks the panel as one whose legibility the downscale test checks. */
  critical?: boolean;
  /** Passed straight through, e.g. `data-testid`. */
  testId?: string;
}

export function Panel({
  box,
  title,
  aside,
  className,
  bodyClassName,
  children,
  critical,
  testId,
}: PanelProps) {
  return (
    <Card
      className={cn('panel', className)}
      {...(testId ? { 'data-testid': testId } : {})}
      style={boxStyle(box)}
      {...(critical ? { 'data-legible': 'critical' } : {})}
    >
      <div className={cn('panel__body', bodyClassName)}>
        {title ? (
          <div className="flex shrink-0 items-baseline justify-between gap-2">
            <span className="panel-title">{title}</span>
            {aside}
          </div>
        ) : null}
        {children}
      </div>
    </Card>
  );
}
