import { useEffect, useState } from "react";
import { messageOf } from "@/api/errors";
import type { S } from "@/api/http";
import { Button, Check2, Field, Notice, NumField, Select, TagInput } from "@/components/ui";
import { browserTimezone, timezones } from "@/lib/format";

type Settings = S["SendingSettings"];

const DAYS = [
  [1, "Mon"],
  [2, "Tue"],
  [3, "Wed"],
  [4, "Thu"],
  [5, "Fri"],
  [6, "Sat"],
  [7, "Sun"],
] as const;

/** Days, hours and time zone. Used by the onboarding step and by the full form. */
export function DaysAndHours({
  value,
  onChange,
}: {
  value: Settings;
  onChange: (patch: Partial<Settings>) => void;
}) {
  const zones = timezones();
  const options = zones.includes(value.timezone) ? zones : [value.timezone, ...zones];
  return (
    <div className="flex flex-col gap-4">
      <Select
        label="Time zone"
        value={value.timezone}
        options={options.map((z) => ({ value: z, label: z }))}
        onChange={(timezone) => onChange({ timezone })}
        help={`Your browser says ${browserTimezone()}.`}
      />
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium">Send days</legend>
        <div className="flex flex-wrap gap-1.5">
          {DAYS.map(([n, label]) => {
            const on = value.send_days.includes(n);
            return (
              <button
                key={n}
                type="button"
                aria-pressed={on}
                onClick={() =>
                  onChange({
                    send_days: on
                      ? value.send_days.filter((d) => d !== n)
                      : [...value.send_days, n].sort(),
                  })
                }
                className={`h-8 min-w-12 rounded-(--radius-control) border px-2 text-sm ${
                  on ? "border-accent bg-accent-soft font-medium" : "border-border text-muted"
                }`}
              >
                {label}
              </button>
            );
          })}
        </div>
      </fieldset>
      <div className="grid grid-cols-2 gap-3">
        <Field
          label="Sending starts"
          type="time"
          value={value.window_start}
          onChange={(e) => onChange({ window_start: e.target.value })}
        />
        <Field
          label="Sending ends"
          type="time"
          value={value.window_end}
          onChange={(e) => onChange({ window_end: e.target.value })}
        />
      </div>
    </div>
  );
}

/** The full sending settings of an account (or the defaults for new accounts). */
export function SendingForm({
  value,
  warnings,
  onSave,
  onReset,
  saveLabel = "Save",
}: {
  value: Settings;
  warnings?: string[] | undefined;
  onSave: (s: Settings) => Promise<unknown>;
  onReset?: (() => Promise<unknown>) | undefined;
  saveLabel?: string;
}) {
  const [s, setS] = useState<Settings>(value);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  useEffect(() => setS(value), [value]);
  const patch = (p: Partial<Settings>) => {
    setS((cur) => ({ ...cur, ...p }));
    setSaved(false);
  };
  async function save() {
    setBusy(true);
    setError(null);
    try {
      await onSave(s);
      setSaved(true);
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }
  const gapMin = Math.round((s.base_delay_secs + s.jitter_min_secs) / 60);
  const gapMax = Math.round((s.base_delay_secs + s.jitter_max_secs) / 60);
  const warmDay = (n: number) => s.warmup_start + s.warmup_step * (n - 1);
  return (
    <div className="flex max-w-3xl flex-col gap-8">
      <Section title="Days and hours">
        <DaysAndHours value={s} onChange={patch} />
        <TagInput
          label="Days off"
          values={s.blocked_dates}
          onChange={(blocked_dates) => patch({ blocked_dates })}
          placeholder="YYYY-MM-DD, then Enter"
          help="No emails go out on these dates."
        />
        <NumField
          label="Start time varies by up to"
          value={s.window_start_jitter_mins}
          min={0}
          max={600}
          suffix="minutes"
          onChange={(n) => patch({ window_start_jitter_mins: n })}
        />
      </Section>

      <Section title="Pace">
        <div className="grid grid-cols-3 gap-3">
          <NumField
            label="Fixed gap"
            value={s.base_delay_secs}
            min={60}
            suffix="s"
            onChange={(n) => patch({ base_delay_secs: n })}
          />
          <NumField
            label="Extra gap, at least"
            value={s.jitter_min_secs}
            min={0}
            suffix="s"
            onChange={(n) => patch({ jitter_min_secs: n })}
          />
          <NumField
            label="Extra gap, at most"
            value={s.jitter_max_secs}
            min={0}
            suffix="s"
            onChange={(n) => patch({ jitter_max_secs: n })}
          />
        </div>
        <p className="text-xs text-muted">
          {gapMin} to {gapMax} minutes between emails.
        </p>
      </Section>

      <Section title="Limits">
        <div className="grid grid-cols-2 gap-3">
          <NumField
            label="Emails per day"
            value={s.daily_limit}
            min={1}
            max={500}
            onChange={(n) => patch({ daily_limit: n })}
          />
          <NumField
            label="Emails per hour"
            value={s.hourly_limit}
            min={1}
            onChange={(n) => patch({ hourly_limit: n })}
          />
          <NumField
            label="Daily limit varies by up to"
            value={s.daily_limit_jitter_pct}
            min={0}
            max={50}
            suffix="%"
            onChange={(n) => patch({ daily_limit_jitter_pct: n })}
          />
          <NumField
            label="Per company domain per day"
            value={s.per_domain_daily_limit}
            min={1}
            onChange={(n) => patch({ per_domain_daily_limit: n })}
          />
          <NumField
            label="Same person cooldown"
            value={s.same_recipient_cooldown_hours}
            min={0}
            suffix="hours"
            onChange={(n) => patch({ same_recipient_cooldown_hours: n })}
          />
        </div>
        <TagInput
          label="Domains without a company limit"
          values={s.per_domain_exempt}
          onChange={(per_domain_exempt) => patch({ per_domain_exempt })}
          help="Free mail domains such as gmail.com are many different people."
        />
      </Section>

      <Section title="Warm-up">
        <Check2
          label="Warm up this inbox"
          help="Start with few emails a day and add more each day."
          checked={s.warmup_enabled}
          onChange={(warmup_enabled) => patch({ warmup_enabled })}
        />
        {s.warmup_enabled && (
          <>
            <div className="grid grid-cols-2 gap-3">
              <NumField
                label="First day"
                value={s.warmup_start}
                min={1}
                suffix="emails"
                onChange={(n) => patch({ warmup_start: n })}
              />
              <NumField
                label="Add each day"
                value={s.warmup_step}
                min={0}
                suffix="emails"
                onChange={(n) => patch({ warmup_step: n })}
              />
            </div>
            <p className="text-xs text-muted">
              Day 1: {Math.min(warmDay(1), s.daily_limit)} emails. Day 4:{" "}
              {Math.min(warmDay(4), s.daily_limit)} emails.
            </p>
          </>
        )}
      </Section>

      <Section title="Safety">
        <div className="grid grid-cols-2 gap-3">
          <NumField
            label="Tries per email"
            value={s.max_attempts}
            min={1}
            onChange={(n) => patch({ max_attempts: n })}
          />
          <NumField
            label="Pause after failures in a row"
            value={s.pause_after_failures}
            min={1}
            onChange={(n) => patch({ pause_after_failures: n })}
          />
          <NumField
            label="Pause when bounce rate is over"
            value={s.pause_bounce_rate_pct}
            min={1}
            max={100}
            suffix="%"
            onChange={(n) => patch({ pause_bounce_rate_pct: n })}
          />
          <NumField
            label="Looking at the last"
            value={s.bounce_window}
            min={1}
            suffix="emails"
            onChange={(n) => patch({ bounce_window: n })}
          />
        </div>
        <Check2
          label="Send follow-ups before new first emails"
          checked={s.followups_first}
          onChange={(followups_first) => patch({ followups_first })}
        />
      </Section>

      <Section title="Unsubscribe header">
        <Field
          label="Unsubscribe email address (mailto)"
          value={s.list_unsubscribe?.mailto ?? ""}
          onChange={(e) =>
            patch({
              list_unsubscribe: { ...(s.list_unsubscribe ?? {}), mailto: e.target.value || null },
            })
          }
        />
        <Field
          label="Unsubscribe web address"
          value={s.list_unsubscribe?.url ?? ""}
          onChange={(e) =>
            patch({
              list_unsubscribe: { ...(s.list_unsubscribe ?? {}), url: e.target.value || null },
            })
          }
        />
      </Section>

      <Section title="Reply check gate">
        <NumField
          label="Follow-ups need an inbox read in the last"
          value={s.reply_check_max_age_mins}
          min={0}
          suffix="minutes"
          help="0 turns the check off."
          onChange={(n) => patch({ reply_check_max_age_mins: n })}
        />
      </Section>

      {(warnings ?? []).map((w) => (
        <Notice key={w} tone="warning">
          {w}
        </Notice>
      ))}
      {error && <Notice tone="danger">{error}</Notice>}
      <div className="flex items-center gap-2">
        <Button busy={busy} onClick={save}>
          {saveLabel}
        </Button>
        {onReset && (
          <Button
            variant="secondary"
            onClick={async () => {
              setBusy(true);
              try {
                await onReset();
              } catch (e) {
                setError(messageOf(e));
              } finally {
                setBusy(false);
              }
            }}
          >
            Reset to defaults
          </Button>
        )}
        {saved && (
          <span className="text-sm text-success" role="status">
            Saved
          </span>
        )}
      </div>
    </div>
  );
}

export function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="flex flex-col gap-4">
      <h2 className="border-b border-border pb-1.5 text-sm font-semibold">{title}</h2>
      {children}
    </section>
  );
}
