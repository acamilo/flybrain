import { Slot } from '@radix-ui/react-slot';
import { cva, type VariantProps } from 'class-variance-authority';
import * as React from 'react';

import { cn } from '@/lib/utils';

/**
 * A chip.
 *
 * `docs/design/gameboy-theme.md`: "Chips (buttons, mode) are boxes with the same double frame,
 * 4 px corner cut instead of radius." `.chip-cut` (`src/theme/panels.css`) carries the cut and both
 * lines of the frame; the frame is 2 px + 2 px rather than the panels' 4 + 2, because the title
 * strip is 40 px and 27 px of type inside a 6 px frame does not fit in it.
 *
 * No `font-semibold`: neither face on the page has a bold, and a synthetic one at the body floor is
 * a smear. A chip that needs emphasis takes the accent border and the accent ink, which every
 * variant but `secondary` already does.
 */
const badgeVariants = cva(
  'chip-cut inline-flex items-center justify-center gap-1 px-2 py-0.5 whitespace-nowrap uppercase tracking-[0.06em]',
  {
    variants: {
      variant: {
        default: 'border-accent bg-transparent text-accent',
        secondary: 'border-bezel bg-bg-2 text-ink-1',
        warn: 'border-warn bg-transparent text-warn',
        alarm: 'border-alarm bg-alarm text-bg-0',
        ok: 'border-ok bg-transparent text-ok',
        outline: 'border-ink-2 bg-transparent text-ink-1',
      },
    },
    defaultVariants: {
      variant: 'default',
    },
  },
);

function Badge({
  className,
  variant,
  asChild = false,
  ...props
}: React.ComponentProps<'span'> & VariantProps<typeof badgeVariants> & { asChild?: boolean }) {
  const Comp = asChild ? Slot : 'span';
  return <Comp data-slot="badge" className={cn(badgeVariants({ variant }), className)} {...props} />;
}

export { Badge, badgeVariants };
