import * as ProgressPrimitive from '@radix-ui/react-progress';
import * as React from 'react';

import { cn } from '@/lib/utils';

/**
 * shadcn/ui Progress. The indicator animates with `transform: translateX`, which stays on the
 * compositor — the one requirement the broadcast page puts on it (design A3: bars never touch
 * layout, and nothing animated carries a shadow or a blur).
 */
function Progress({
  className,
  value,
  indicatorClassName,
  ...props
}: React.ComponentProps<typeof ProgressPrimitive.Root> & { indicatorClassName?: string }) {
  return (
    <ProgressPrimitive.Root
      data-slot="progress"
      className={cn('relative w-full overflow-hidden rounded-[3px] bg-bg-0', className)}
      value={value}
      {...props}
    >
      <ProgressPrimitive.Indicator
        data-slot="progress-indicator"
        className={cn('h-full w-full flex-1 bg-accent transition-transform', indicatorClassName)}
        style={{
          transform: `translateX(-${100 - (value ?? 0)}%)`,
          transitionDuration: 'var(--bar-ms)',
        }}
      />
    </ProgressPrimitive.Root>
  );
}

export { Progress };
