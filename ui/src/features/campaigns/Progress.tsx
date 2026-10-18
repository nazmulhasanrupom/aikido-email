import type { S } from "@/api/http";
import { cn } from "@/lib/cn";

/** Leads that ended their sequence, out of all leads that are in it or waiting for it. */
export function progressOf(counts: S["Counts"]): number | null {
  const e = counts.enrollments;
  const done = (e.finished ?? 0) + (e.stopped ?? 0) + (e.failed ?? 0);
  const total = counts.enrolled + counts.pending;
  return total > 0 ? Math.min(1, done / total) : null;
}

export function ProgressBar({ value, className }: { value: number; className?: string }) {
  const pct = Math.round(value * 100);
  return (
    <div className={cn("flex items-center gap-2.5", className)}>
      <span className="w-9 text-right text-sm font-medium tabular-nums">{pct}%</span>
      <div
        role="progressbar"
        aria-valuenow={pct}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label="Campaign progress"
        className="h-1.5 w-24 overflow-hidden rounded-full bg-surface-2"
      >
        <div className="h-full rounded-full bg-accent" style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}
