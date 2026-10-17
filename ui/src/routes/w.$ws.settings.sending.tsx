import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import {
  Button,
  Card,
  Check2,
  ErrorNote,
  Notice,
  NumField,
  Select,
  Skeleton,
  TagInput,
} from "@/components/ui";
import { ReplyForm } from "@/features/settings/ReplyForm";
import { SendingForm } from "@/features/settings/SendingForm";
import { useUser } from "@/features/settings/useUser";

export const Route = createFileRoute("/w/$ws/settings/sending")({ component: SendingSettings });

function SendingSettings() {
  const { isAdmin } = useUser();
  const qc = useQueryClient();
  const defaults = useQuery({ ...q.sendingDefaults, enabled: isAdmin });
  const replies = useQuery({
    queryKey: ["replies", "defaults"],
    enabled: isAdmin,
    queryFn: () => http.get<{ settings: S["ReplySettings"] }>("/v1/replies/defaults"),
  });
  if (!isAdmin)
    return <Notice tone="warning">Only an owner or an admin can change these settings.</Notice>;
  return (
    <div className="flex flex-col gap-10">
      <section className="flex flex-col gap-3">
        <h2 className="text-lg font-semibold">Defaults for new accounts</h2>
        <p className="text-muted">An account keeps its own settings once you change them there.</p>
        {defaults.isPending && <Skeleton />}
        {defaults.isError && <ErrorNote error={defaults.error} />}
        {defaults.data && (
          <SendingForm
            value={defaults.data.settings}
            warnings={defaults.data.warnings}
            onSave={async (s) => {
              qc.setQueryData(
                q.sendingDefaults.queryKey,
                await http.patch<S["SettingsReply"]>("/v1/sending/defaults", { body: s }),
              );
            }}
          />
        )}
      </section>
      <section className="flex flex-col gap-3">
        <h2 className="text-lg font-semibold">Reply check defaults</h2>
        {replies.isPending && <Skeleton />}
        {replies.data && (
          <ReplyForm
            value={replies.data.settings}
            onSave={async (s) => {
              qc.setQueryData(
                ["replies", "defaults"],
                await http.patch<{ settings: S["ReplySettings"] }>("/v1/replies/defaults", {
                  body: s,
                }),
              );
            }}
          />
        )}
      </section>
      <FollowupBlock />
      <LeadsBlock />
    </div>
  );
}

function FollowupBlock() {
  const res = useQuery({
    queryKey: ["followups", "settings"],
    queryFn: () => http.get<{ settings: S["FollowupSettings"] }>("/v1/followups/settings"),
  });
  const [s, setS] = useState<S["FollowupSettings"] | null>(null);
  useEffect(() => {
    if (res.data) setS(res.data.settings);
  }, [res.data]);
  if (!s) return null;
  return (
    <Card className="flex max-w-2xl flex-col gap-3">
      <h2 className="font-semibold">Follow-ups</h2>
      <Check2
        label="One active sequence per email address"
        checked={s.one_active_per_email}
        onChange={(v) => setS({ ...s, one_active_per_email: v })}
      />
      <NumField
        label="Longest wait between steps"
        value={s.max_wait_days}
        min={1}
        suffix="days"
        onChange={(n) => setS({ ...s, max_wait_days: n })}
      />
      <NumField
        label="Engine speed: check every"
        value={s.tick_secs}
        min={5}
        suffix="seconds"
        onChange={(n) => setS({ ...s, tick_secs: n })}
      />
      <div>
        <Button
          onClick={async () => {
            try {
              await http.patch("/v1/followups/settings", { body: s });
              toast.success("Saved.");
            } catch (e) {
              toast.error(messageOf(e));
            }
          }}
        >
          Save
        </Button>
      </div>
    </Card>
  );
}

function LeadsBlock() {
  const res = useQuery(q.leadSettings);
  const qc = useQueryClient();
  const [s, setS] = useState<S["LeadSettings"] | null>(null);
  useEffect(() => {
    if (res.data) setS(res.data.settings);
  }, [res.data]);
  if (!s) return null;
  return (
    <Card className="flex max-w-2xl flex-col gap-3">
      <h2 className="font-semibold">Leads</h2>
      <div className="grid grid-cols-2 gap-3">
        <NumField
          label="Biggest import file"
          value={Math.round(s.import_max_bytes / 1_048_576)}
          min={1}
          suffix="MB"
          onChange={(n) => setS({ ...s, import_max_bytes: n * 1_048_576 })}
        />
        <NumField
          label="Most rows in one import"
          value={s.import_max_rows}
          min={1}
          onChange={(n) => setS({ ...s, import_max_rows: n })}
        />
        <NumField
          label="New leads per day for new campaigns"
          value={s.default_daily_new_leads}
          min={1}
          onChange={(n) => setS({ ...s, default_daily_new_leads: n })}
        />
        <NumField
          label="Feeder speed: check every"
          value={s.feeder_tick_secs}
          min={5}
          suffix="seconds"
          onChange={(n) => setS({ ...s, feeder_tick_secs: n })}
        />
      </div>
      <Select
        label="If we already have the lead"
        value={s.default_dedupe_mode}
        onChange={(v) => setS({ ...s, default_dedupe_mode: v as S["DedupeMode"] })}
        options={[
          { value: "fill_empty", label: "Fill empty fields" },
          { value: "skip", label: "Skip" },
          { value: "overwrite", label: "Overwrite" },
        ]}
      />
      <Check2
        label="Check that the domain of an email can get mail"
        checked={s.mx_check}
        onChange={(v) => setS({ ...s, mx_check: v })}
      />
      <TagInput
        label="Extra free mail domains"
        values={s.free_mail_domains_extra}
        onChange={(v) => setS({ ...s, free_mail_domains_extra: v })}
        help="These are never treated as one company."
      />
      <div>
        <Button
          onClick={async () => {
            try {
              qc.setQueryData(
                q.leadSettings.queryKey,
                await http.patch<{ settings: S["LeadSettings"] }>("/v1/leads/settings", {
                  body: s,
                }),
              );
              toast.success("Saved.");
            } catch (e) {
              toast.error(messageOf(e));
            }
          }}
        >
          Save
        </Button>
      </div>
    </Card>
  );
}
