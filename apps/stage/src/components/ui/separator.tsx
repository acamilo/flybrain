import * as SeparatorPrimitive from '@radix-ui/react-separator';
import * as React from 'react';

import { cn } from '@/lib/utils';

/**
 * shadcn/ui Separator, thickened to 2 px: x264 at 3000 kbps erases 1 px hairlines (the audit
 * finding), so the broadcast page has no hairlines anywhere.
 */
function Separator({
  className,
  orientation = 'horizontal',
  decorative = true,
  ...props
}: React.ComponentProps<typeof SeparatorPrimitive.Root>) {
  return (
    <SeparatorPrimitive.Root
      data-slot="separator"
      decorative={decorative}
      orientation={orientation}
      className={cn(
        'shrink-0 bg-bezel',
        orientation === 'horizontal' ? 'h-[2px] w-full' : 'h-full w-[2px]',
        className,
      )}
      {...props}
    />
  );
}

export { Separator };
