import { cn } from "@/lib/cn";

/**
 * An ensō: the brush circle of Zen ink painting. The mark of the Indigo design.
 * The ring is drawn from two circles with a small offset, so the stroke is thick at the bottom left,
 * starts round at the top and ends in a point where the brush lifts. It takes the current text color.
 */
export function Enso({ className, size = 96 }: { className?: string; size?: number }) {
  return (
    <svg viewBox="0 0 100 100" width={size} height={size} aria-hidden="true" className={cn("shrink-0", className)}>
      <path
        fill="currentColor"
        d="M63.68 12.41 A40 40 0 1 0 86.25 33.10 Q86.6 43.5 83.91 55.28 A32 32 0 1 1 63.94 16.93 Q66.6 14.2 63.68 12.41 Z"
      />
      {/* thin lines along the edge, like dry ink at the side of the brush */}
      <g fill="none" stroke="currentColor" strokeLinecap="round">
        <path d="M57.5 10.4 A40.6 40.6 0 0 0 11.2 58.0" strokeWidth="0.9" opacity="0.45" />
        <path d="M14.6 72.0 A41 41 0 0 0 43.0 90.6" strokeWidth="0.7" opacity="0.35" />
      </g>
    </svg>
  );
}
