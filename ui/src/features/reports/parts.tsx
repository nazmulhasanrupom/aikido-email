import { useState } from "react";
import { Field } from "@/components/ui";
import { cn } from "@/lib/cn";

export interface Range {
  from: string;
  to: string;
  preset: "7" | "30" | "all" | "custom";
}

const iso = (d: Date) => {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
};
export function presetRange(preset: "7" | "30" | "all"): Range {
  const to = new Date();
  const from = new Date();
  from.setDate(to.getDate() - (preset === "7" ? 6 : preset === "30" ? 29 : 365));
  return { from: iso(from), to: iso(to), preset };
}

export function useRange(initial: "7" | "30" | "all" = "30") {
  return useState<Range>(() => presetRange(initial));
}

export function RangePicker({ value, onChange }: { value: Range; onChange: (r: Range) => void }) {
  const items: [Range["preset"], string][] = [
    ["7", "Last 7 days"],
    ["30", "Last 30 days"],
    ["all", "All"],
    ["custom", "Custom"],
  ];
  return (
    <div className="flex flex-wrap items-end gap-2" role="group" aria-label="Date range">
      {items.map(([k, label]) => (
        <button
          key={k}
          type="button"
          aria-pressed={value.preset === k}
          onClick={() => onChange(k === "custom" ? { ...value, preset: "custom" } : presetRange(k))}
          className={cn(
            "h-9 rounded-(--radius-control) border px-3 text-sm",
            value.preset === k ? "border-accent bg-accent-soft font-medium" : "border-border",
          )}
        >
          {label}
        </button>
      ))}
      {value.preset === "custom" && (
        <>
          <Field
            label="From"
            type="date"
            value={value.from}
            max={value.to}
            onChange={(e) => onChange({ ...value, from: e.target.value })}
          />
          <Field
            label="To"
            type="date"
            value={value.to}
            min={value.from}
            onChange={(e) => onChange({ ...value, to: e.target.value })}
          />
        </>
      )}
    </div>
  );
}

export function StatCard({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="rounded-(--radius-card) border border-border bg-surface p-4">
      <p className="text-xs text-muted">{label}</p>
      <p className="mt-1 text-2xl font-semibold">{value}</p>
      {sub && <p className="text-xs text-muted">{sub}</p>}
    </div>
  );
}

export interface Series {
  name: string;
  color: string;
  values: number[];
}

/** A small line chart in SVG. The data is also in a table below it, for screen readers. */
export function LineChart({
  labels,
  series,
  height = 200,
}: {
  labels: string[];
  series: Series[];
  height?: number;
}) {
  const W = 800;
  const H = height;
  const pad = { l: 36, r: 8, t: 8, b: 22 };
  const max = Math.max(1, ...series.flatMap((s) => s.values));
  const n = Math.max(1, labels.length - 1);
  const x = (i: number) => pad.l + (i / n) * (W - pad.l - pad.r);
  const y = (v: number) => pad.t + (1 - v / max) * (H - pad.t - pad.b);
  const ticks = [0, 0.5, 1].map((t) => Math.round(max * t));
  return (
    <figure className="m-0">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        className="w-full"
        role="img"
        aria-label={`Chart of ${series.map((s) => s.name).join(" and ")} per day`}
      >
        {ticks.map((t) => (
          <g key={t}>
            <line x1={pad.l} x2={W - pad.r} y1={y(t)} y2={y(t)} stroke="var(--border)" />
            <text
              x={pad.l - 6}
              y={y(t) + 4}
              textAnchor="end"
              fontSize="11"
              fill="var(--text-muted)"
            >
              {t}
            </text>
          </g>
        ))}
        {labels.length > 0 &&
          [0, Math.floor(labels.length / 2), labels.length - 1].map((i) => (
            <text
              key={i}
              x={x(i)}
              y={H - 6}
              textAnchor={i === 0 ? "start" : i === labels.length - 1 ? "end" : "middle"}
              fontSize="11"
              fill="var(--text-muted)"
            >
              {labels[i]}
            </text>
          ))}
        {series.map((s) => (
          <polyline
            key={s.name}
            fill="none"
            stroke={s.color}
            strokeWidth="2"
            strokeLinejoin="round"
            points={s.values.map((v, i) => `${x(i)},${y(v)}`).join(" ")}
          />
        ))}
      </svg>
      <figcaption className="mt-1 flex gap-4 text-xs text-muted">
        {series.map((s) => (
          <span key={s.name} className="flex items-center gap-1.5">
            <span className="inline-block h-0.5 w-4" style={{ background: s.color }} />
            {s.name}
          </span>
        ))}
      </figcaption>
      <table className="sr-only">
        <caption>Numbers per day</caption>
        <thead>
          <tr>
            <th>Day</th>
            {series.map((s) => (
              <th key={s.name}>{s.name}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {labels.map((l, i) => (
            <tr key={l}>
              <td>{l}</td>
              {series.map((s) => (
                <td key={s.name}>{s.values[i]}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </figure>
  );
}
