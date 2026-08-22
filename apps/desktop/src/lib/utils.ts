import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * Join class names, letting a later one win over an earlier one.
 *
 * `clsx` handles the conditionals; `twMerge` resolves the conflicts, so a
 * component's own `px-3` can be overridden by a caller's `px-2` instead of
 * both landing in the class list and the cascade deciding by declaration
 * order — which is not something the caller can see.
 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
