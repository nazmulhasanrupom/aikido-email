import { useEffect, useState } from "react";
import { messageOf } from "@/api/errors";
import type { S } from "@/api/http";
import { Button, Check2, Notice, NumField, Select, TagInput } from "@/components/ui";
import { Section } from "./SendingForm";

type Settings = S["ReplySettings"];

export function ReplyForm({
  value,
  onSave,
  onReset,
}: {
  value: Settings;
  onSave: (s: Settings) => Promise<unknown>;
  onReset?: (() => Promise<unknown>) | undefined;
}) {
  const [s, setS] = useState(value);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  useEffect(() => setS(value), [value]);
  const patch = (p: Partial<Settings>) => {
    setS((c) => ({ ...c, ...p }));
    setSaved(false);
  };
  async function run(fn: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await fn();
      setSaved(true);
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="flex max-w-3xl flex-col gap-8">
      <Section title="Reading the inbox">
        <Check2
          label="Check this inbox for replies"
          checked={s.enabled}
          onChange={(enabled) => patch({ enabled })}
        />
        <div className="grid grid-cols-2 gap-3">
          <NumField
            label="Check every"
            value={Math.round(s.poll_interval_secs / 60)}
            min={1}
            suffix="minutes"
            onChange={(n) => patch({ poll_interval_secs: n * 60 })}
          />
          <NumField
            label="Look back on the first read"
            value={s.initial_lookback_days}
            min={1}
            suffix="days"
            onChange={(n) => patch({ initial_lookback_days: n })}
          />
          <NumField
            label="Match a reply to a send within"
            value={s.match_window_days}
            min={1}
            suffix="days"
            onChange={(n) => patch({ match_window_days: n })}
          />
          <NumField
            label="Emails per read, at most"
            value={s.max_messages_per_sync}
            min={1}
            onChange={(n) => patch({ max_messages_per_sync: n })}
          />
        </div>
        <TagInput
          label="Folders"
          values={s.folders}
          onChange={(folders) => patch({ folders })}
          help="For example inbox and spam."
        />
      </Section>
      <Section title="What happens">
        <Select
          label="On an out-of-office reply"
          value={s.on_auto_reply}
          options={[
            { value: "ignore", label: "Pause the sequence, then go on" },
            { value: "stop", label: "Stop the sequence" },
          ]}
          onChange={(v) => patch({ on_auto_reply: v as Settings["on_auto_reply"] })}
        />
        <Select
          label="Who is blocked after an unsubscribe or a bounce"
          value={s.stop_scope}
          options={[
            { value: "email", label: "Only that address" },
            { value: "domain", label: "The whole company domain" },
          ]}
          onChange={(v) => patch({ stop_scope: v as Settings["stop_scope"] })}
        />
        <NumField
          label="Soft bounces before a block"
          value={s.soft_bounce_limit}
          min={1}
          onChange={(n) => patch({ soft_bounce_limit: n })}
        />
        <Check2
          label="Count blocked-by-server bounces in the bounce rate"
          checked={s.count_blocks_in_bounce_rate}
          onChange={(count_blocks_in_bounce_rate) => patch({ count_blocks_in_bounce_rate })}
        />
      </Section>
      <Section title="Keyword lists">
        <TagInput
          label="Unsubscribe words"
          values={s.unsubscribe_keywords}
          onChange={(unsubscribe_keywords) => patch({ unsubscribe_keywords })}
        />
        <NumField
          label="Unsubscribe only counts in replies up to"
          value={s.unsubscribe_max_words}
          min={1}
          suffix="words"
          onChange={(n) => patch({ unsubscribe_max_words: n })}
        />
        <TagInput
          label="Out-of-office subject words"
          values={s.auto_reply_subject_patterns}
          onChange={(auto_reply_subject_patterns) => patch({ auto_reply_subject_patterns })}
        />
        <TagInput
          label="Bounce subject words"
          values={s.bounce_subject_patterns}
          onChange={(bounce_subject_patterns) => patch({ bounce_subject_patterns })}
        />
      </Section>
      {error && <Notice tone="danger">{error}</Notice>}
      <div className="flex items-center gap-2">
        <Button busy={busy} onClick={() => run(() => onSave(s))}>
          Save
        </Button>
        {onReset && (
          <Button variant="secondary" onClick={() => run(onReset)}>
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
