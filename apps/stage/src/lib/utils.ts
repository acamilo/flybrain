import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

/** shadcn/ui's class merge helper (`components.json` points `@/lib/utils` here). */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
