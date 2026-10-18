import { ChevronRight } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { messageOf } from "@/api/errors";
import type { S } from "@/api/http";
import {
  Button,
  Check2,
  Field,
  Notice,
  NumField,
  Select,
  TagInput,
  TextArea,
} from "@/components/ui";
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
  const [open, setOpen] = useState<Record<string, boolean>>({ days: true });
  useEffect(() => setS(value), [value]);
  const patch = (p: Partial<Settings>) => {
    setS((cur) => ({ ...cur, ...p }));
    setSaved(false);
  };
  const dirty = JSON.stringify(s) !== JSON.stringify(value);
  async function save() {
    setBusy(true);
    setError(null);
    try {
      const lu = s.list_unsubscribe;
      await onSave({ ...s, list_unsubscribe: lu?.mailto || lu?.url ? lu : null });
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
  const days = DAYS.filter(([n]) => s.send_days.includes(n))
    .map(([, l]) => l)
    .join(", ");
  const ids = ["days", "pace", "limits", "warmup", "signature", "unsub", "safety", "gate"];
  const allOpen = ids.every((k) => open[k]);
  const part = (id: string) => ({
    open: !!open[id],
    onToggle: () => setOpen((o) => ({ ...o, [id]: !o[id] })),
  });
  return (
    <div className="flex max-w-3xl flex-col gap-3">
      <div className="flex justify-end">
        <button
          type="button"
          className="text-sm text-accent hover:underline"
          onClick={() => setOpen(Object.fromEntries(ids.map((k) => [k, !allOpen])))}
        >
          {allOpen ? "Close all" : "Open all"}
        </button>
      </div>

      <Part
        title="Days and hours"
        summary={`${days || "No days"} · ${s.window_start}–${s.window_end} · ${s.timezone}`}
        {...part("days")}
      >
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
      </Part>

      <Part title="Pace" summary={`${gapMin} to ${gapMax} minutes between emails`} {...part("pace")}>
        <div className="grid gap-3 sm:grid-cols-3">
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
      </Part>

      <Part
        title="Limits"
        summary={`${s.daily_limit} a day · ${s.hourly_limit} an hour · ${s.per_domain_daily_limit} per company`}
        {...part("limits")}
      >
        <div className="grid gap-3 sm:grid-cols-2">
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
      </Part>

      <Part
        title="Warm-up"
        summary={
          s.warmup_enabled ? `On · ${s.warmup_start} first day, +${s.warmup_step} each day` : "Off"
        }
        badge={s.warmup_enabled ? "On" : undefined}
        {...part("warmup")}
      >
        <Check2
          label="Warm up this inbox"
          help="Start with few emails a day and add more each day."
          checked={s.warmup_enabled}
          onChange={(warmup_enabled) => patch({ warmup_enabled })}
        />
        {s.warmup_enabled && (
          <>
            <div className="grid gap-3 sm:grid-cols-2">
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
      </Part>

      <Part
        title="Signature"
        summary={s.signature_html.trim() ? "Added under every email" : "None"}
        badge={s.signature_html.trim() ? "On" : undefined}
        {...part("signature")}
      >
        <SignatureEditor
          value={s.signature_html}
          onChange={(signature_html) => patch({ signature_html })}
        />
      </Part>

      <Part
        title="Unsubscribe link"
        summary="{{unsubscribe}} is made for every email"
        {...part("unsub")}
      >
        <div className="flex flex-col gap-1.5 text-sm">
          <p>
            Put <Code>{"{{unsubscribe}}"}</Code> in an email or in the signature. We put a link
            there that is made for each email. One click unsubscribes the person.
          </p>
          <p className="text-muted">
            <Code>{"{{unsubscribe_url}}"}</Code> gives the address only, for your own link:{" "}
            <Code>{'<a href="{{unsubscribe_url}}">Stop these emails</a>'}</Code>
          </p>
          <p className="text-muted">
            The server needs AIKIDO_PUBLIC_URL for a web link. Without it, the link is an email
            to this inbox, and the reply check sees the unsubscribe.
          </p>
        </div>
        <details className="rounded-(--radius-control) border border-border p-3">
          <summary className="cursor-pointer text-sm font-medium">
            Own List-Unsubscribe header (optional)
          </summary>
          <div className="mt-3 flex flex-col gap-3">
            <p className="text-xs text-muted">
              Empty: the header uses the link above (https only). Fill these to use your own.
            </p>
            <Field
              label="Unsubscribe email address (mailto)"
              value={s.list_unsubscribe?.mailto ?? ""}
              onChange={(e) =>
                patch({
                  list_unsubscribe: {
                    ...(s.list_unsubscribe ?? {}),
                    mailto: e.target.value || null,
                  },
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
          </div>
        </details>
      </Part>

      <Part
        title="Safety"
        summary={`${s.max_attempts} tries · pause at ${s.pause_bounce_rate_pct}% bounces`}
        {...part("safety")}
      >
        <div className="grid gap-3 sm:grid-cols-2">
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
      </Part>

      <Part
        title="Reply check gate"
        summary={
          s.reply_check_max_age_mins > 0
            ? `Follow-ups need an inbox read in the last ${s.reply_check_max_age_mins} min`
            : "Off"
        }
        {...part("gate")}
      >
        <NumField
          label="Follow-ups need an inbox read in the last"
          value={s.reply_check_max_age_mins}
          min={0}
          suffix="minutes"
          help="0 turns the check off."
          onChange={(n) => patch({ reply_check_max_age_mins: n })}
        />
      </Part>

      {(warnings ?? []).map((w) => (
        <Notice key={w} tone="warning">
          {w}
        </Notice>
      ))}
      {error && <Notice tone="danger">{error}</Notice>}
      <div className="sticky bottom-0 z-10 -mx-1 flex items-center gap-2 border-t border-border bg-bg/95 px-1 py-3 backdrop-blur">
        <Button busy={busy} onClick={save} disabled={!dirty && !busy}>
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
        {dirty && !busy && <span className="text-sm text-warning">Not saved</span>}
        {saved && !dirty && (
          <span className="text-sm text-success" role="status">
            Saved
          </span>
        )}
      </div>
    </div>
  );
}

function Code({ children }: { children: React.ReactNode }) {
  return <code className="rounded bg-surface-2 px-1 py-0.5 font-mono text-xs">{children}</code>;
}

/** One part of the form that opens and closes. The summary shows while it is closed. */
function Part({
  title,
  summary,
  badge,
  open,
  onToggle,
  children,
}: {
  title: string;
  summary: string;
  badge?: string | undefined;
  open: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}) {
  const id = useId();
  return (
    <section className="rounded-(--radius-card) border border-border bg-surface">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={id}
        onClick={onToggle}
        className="flex w-full items-center gap-3 px-4 py-3 text-left hover:bg-surface-2/50"
      >
        <ChevronRight
          size={16}
          className={`shrink-0 text-muted transition-transform ${open ? "rotate-90" : ""}`}
        />
        <span className="font-semibold">{title}</span>
        {badge && (
          <span className="rounded-full bg-accent-soft px-2 py-0.5 text-xs font-medium">
            {badge}
          </span>
        )}
        <span className="ml-auto truncate text-right text-sm text-muted">{summary}</span>
      </button>
      {open && (
        <div id={id} className="flex flex-col gap-4 border-t border-border px-4 py-4">
          {children}
        </div>
      )}
    </section>
  );
}

const SAMPLE = { name: "Sam Sender", email: "sam@example.com" };

function escapeHtml(t: string) {
  return t.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** The signature as the email shows it, with sample values. Mirrors `sending::unsubscribe::fill_html`. */
export function signaturePreview(html: string) {
  return html
    .replaceAll("{{sender_name}}", escapeHtml(SAMPLE.name))
    .replaceAll("{{sender_email}}", escapeHtml(SAMPLE.email))
    .replaceAll("{{unsubscribe_url}}", "#unsubscribe")
    .replace(/(["'=]\s*)?\{\{unsubscribe\}\}/g, (_m, attr?: string) =>
      attr ? `${attr}#unsubscribe` : '<a href="#unsubscribe">Unsubscribe</a>',
    );
}

/** HTML signature with buttons for the tokens, and a live preview. */
function SignatureEditor({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const area = useRef<HTMLTextAreaElement>(null);
  function insert(token: string) {
    const el = area.current;
    if (!el) return onChange(value + token);
    const at = el.selectionStart ?? value.length;
    const end = el.selectionEnd ?? at;
    onChange(value.slice(0, at) + token + value.slice(end));
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(at + token.length, at + token.length);
    });
  }
  const doc = `<!doctype html><html><head><meta charset="utf-8"><style>body{font-family:Arial,sans-serif;font-size:14px;color:#222;margin:12px}a{color:#1a5fd0}</style></head><body><p style="color:#888">Hi Alex,<br><br>… the email text …</p>${signaturePreview(value)}</body></html>`;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="text-xs text-muted">Insert:</span>
        {[
          ["{{sender_name}}", "Sender name"],
          ["{{sender_email}}", "Sender email"],
          ["{{unsubscribe}}", "Unsubscribe link"],
        ].map(([t, l]) => (
          <button
            key={t}
            type="button"
            onClick={() => insert(t as string)}
            className="h-7 rounded-(--radius-control) border border-border px-2 text-xs hover:bg-surface-2"
          >
            {l}
          </button>
        ))}
        {!value.trim() && (
          <button
            type="button"
            onClick={() => onChange(SIGNATURE_START)}
            className="ml-auto text-xs text-accent hover:underline"
          >
            Use a starter signature
          </button>
        )}
      </div>
      <div className="grid gap-3 lg:grid-cols-2">
        <TextArea
          ref={area}
          label="Signature HTML"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="min-h-56 font-mono text-xs"
          spellCheck={false}
          placeholder="<p>Best,<br><b>{{sender_name}}</b></p>"
          help="Added under every email from this inbox. Plain text emails get an HTML part too."
        />
        <div className="flex flex-col gap-1.5">
          <span className="text-sm font-medium">Preview</span>
          <iframe
            title="Signature preview"
            sandbox=""
            srcDoc={doc}
            className="min-h-56 w-full flex-1 rounded-(--radius-control) border border-border bg-white"
          />
          <p className="text-xs text-muted">Sample values. The real link is made when it sends.</p>
        </div>
      </div>
    </div>
  );
}

const SIGNATURE_START = `<p style="margin:0">Best,<br><b>{{sender_name}}</b><br>
<a href="mailto:{{sender_email}}">{{sender_email}}</a></p>
<p style="margin-top:12px;font-size:12px;color:#888">Not for you? <a href="{{unsubscribe_url}}">Unsubscribe</a></p>`;

export function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="flex flex-col gap-4">
      <h2 className="border-b border-border pb-1.5 text-sm font-semibold">{title}</h2>
      {children}
    </section>
  );
}
