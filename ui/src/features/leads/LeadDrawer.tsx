import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import {
  Button,
  ErrorNote,
  Field,
  Modal,
  Pill,
  Select,
  Skeleton,
  Tabs,
  TextArea,
} from "@/components/ui";
import { fmtAgo, fmtDateTime, fullName } from "@/lib/format";
import { INTERESTS, say } from "@/lib/words";

type Tab = "details" | "timeline" | "emails" | "notes";
const BUILTIN: [keyof S["Lead"], string][] = [
  ["first_name", "First name"],
  ["last_name", "Last name"],
  ["company_name", "Company"],
  ["job_title", "Job title"],
  ["phone", "Phone"],
  ["website", "Website"],
  ["linkedin_url", "LinkedIn"],
  ["location", "Location"],
  ["timezone", "Time zone"],
];

/** Plain words for a timeline event. */
export function eventText(e: S["LeadEvent"]): string {
  const d = (e.detail ?? {}) as Record<string, unknown>;
  const why = typeof d.reason === "string" ? ` (${say("stop", d.reason)})` : "";
  const names: Record<string, string> = {
    created: "Lead added",
    updated: "Lead changed",
    enrolled: "Put in a sequence",
    sent: "Email sent",
    replied: "Replied",
    bounced: "Email bounced",
    unsubscribed: "Unsubscribed",
    complained: "Marked as spam",
    stopped: `Sequence stopped${why}`,
    completed: "Sequence finished",
    note: `Note: ${String(d.text ?? "")}`,
    interest_changed: `Interest set: ${say("interest", String(d.interest ?? d.to ?? ""))}`,
  };
  return names[e.kind] ?? e.kind.replaceAll("_", " ");
}

export function LeadDrawer({
  id,
  ws,
  onClose,
}: {
  id: string | null;
  ws: string;
  onClose: () => void;
}) {
  const qc = useQueryClient();
  const [tab, setTab] = useState<Tab>("details");
  const ctx = useQuery({
    queryKey: ["leads", "context", id],
    enabled: !!id,
    queryFn: () =>
      http.get<S["LeadContext"]>("/v1/leads/{id}/context", { path: { id: id as string } }),
  });
  useEffect(() => setTab("details"), [id]);
  const lead = ctx.data?.lead;
  const setInterest = useMutation({
    mutationFn: (interest: string) =>
      http.patch(`/v1/leads/{id}`, {
        path: { id: id as string },
        body: { interest: interest || null },
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["leads"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  return (
    <Modal open={!!id} onClose={onClose} side title={lead ? fullName(lead) || lead.email : "Lead"}>
      {ctx.isPending && <Skeleton rows={6} />}
      {ctx.isError && <ErrorNote error={ctx.error} />}
      {lead && ctx.data && (
        <div className="flex flex-col gap-4">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <p className="font-medium">{lead.email}</p>
              <p className="text-muted">
                {[lead.job_title, lead.company_name].filter(Boolean).join(" at ")}
              </p>
            </div>
            <div className="w-44">
              <Select
                label="Interest"
                value={lead.interest ?? ""}
                onChange={(v) => setInterest.mutate(v)}
                options={[
                  { value: "", label: "Not set" },
                  ...INTERESTS.map((i) => ({ value: i, label: say("interest", i) })),
                ]}
              />
            </div>
          </div>
          <Tabs
            label="Lead pages"
            value={tab}
            onChange={setTab}
            items={[
              { value: "details", label: "Details" },
              { value: "timeline", label: "Timeline" },
              { value: "emails", label: "Emails" },
              { value: "notes", label: "Notes" },
            ]}
          />
          {tab === "details" && <Details lead={lead} fields={ctx.data.fields} />}
          {tab === "timeline" && (
            <ul className="flex flex-col gap-2 text-sm">
              {ctx.data.events.length === 0 && <li className="text-muted">Nothing yet.</li>}
              {ctx.data.events.map((e) => (
                <li key={e.id} className="flex justify-between gap-4 border-b border-border pb-1.5">
                  <span>{eventText(e)}</span>
                  <span className="shrink-0 text-muted">{fmtAgo(e.at)}</span>
                </li>
              ))}
            </ul>
          )}
          {tab === "emails" && (
            <ul className="flex flex-col gap-3">
              {ctx.data.messages.length === 0 && <li className="text-muted">No emails yet.</li>}
              {ctx.data.messages.map((m) => (
                <li
                  key={`${m.at}-${m.subject}`}
                  className="rounded-(--radius-control) border border-border p-3 text-sm"
                >
                  <div className="mb-1 flex justify-between text-xs text-muted">
                    <span>
                      {m.direction === "out" || m.direction === "sent" ? "Sent" : "Received"}
                      {m.step_number ? ` (step ${m.step_number})` : ""}
                      {m.label ? `, ${say("label", m.label)}` : ""}
                    </span>
                    <span>{fmtDateTime(m.at)}</span>
                  </div>
                  <p className="font-medium">{m.subject}</p>
                  <p className="whitespace-pre-wrap text-muted">{m.text}</p>
                </li>
              ))}
            </ul>
          )}
          {tab === "notes" && <Notes id={lead.id} notes={lead.notes} />}
          <footer className="flex flex-col gap-1.5 border-t border-border pt-3 text-sm">
            {lead.suppressed && (
              <div>
                <Pill tone="danger">Blocked</Pill>
              </div>
            )}
            <p>
              <span className="text-muted">Lists:</span>{" "}
              {lead.lists.map((l) => l.name).join(", ") || "none"}
            </p>
            <p>
              <span className="text-muted">Tags:</span> {lead.tags.join(", ") || "none"}
            </p>
            <p>
              <span className="text-muted">Campaigns:</span>{" "}
              {lead.campaigns.length
                ? lead.campaigns.map((c) => (
                    <Link
                      key={c.id}
                      to="/w/$ws/campaigns/$id"
                      params={{ ws, id: c.id }}
                      className="mr-2 underline"
                    >
                      {c.name}
                    </Link>
                  ))
                : "none"}
            </p>
          </footer>
        </div>
      )}
    </Modal>
  );
}

function Details({ lead, fields }: { lead: S["LeadDetail"]; fields: S["ContextField"][] }) {
  const qc = useQueryClient();
  const locked = !!lead.active_enrollment_id;
  const [vals, setVals] = useState<Record<string, string>>({});
  const [custom, setCustom] = useState<Record<string, string>>({});
  useEffect(() => {
    setVals(Object.fromEntries(BUILTIN.map(([k]) => [k, String(lead[k] ?? "")])));
    setCustom(Object.fromEntries(fields.map((f) => [f.key, f.value ?? ""])));
  }, [lead, fields]);
  const save = useMutation({
    mutationFn: () =>
      http.patch(`/v1/leads/{id}`, {
        path: { id: lead.id },
        body: {
          ...Object.fromEntries(Object.entries(vals).map(([k, v]) => [k, v || null])),
          custom_fields: Object.fromEntries(Object.entries(custom).filter(([, v]) => v !== "")),
        },
      }),
    onSuccess: () => {
      toast.success("Saved.");
      void qc.invalidateQueries({ queryKey: ["leads"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  return (
    <div className="grid gap-3 sm:grid-cols-2">
      <Field
        label="Email"
        value={lead.email}
        disabled
        help={locked ? "The email is locked while the lead is in a sequence." : undefined}
        readOnly
      />
      {BUILTIN.map(([k, label]) => (
        <Field
          key={k}
          label={label}
          value={vals[k] ?? ""}
          onChange={(e) => setVals({ ...vals, [k]: e.target.value })}
        />
      ))}
      {fields.map((f) => (
        <Field
          key={f.key}
          label={f.label || f.key}
          value={custom[f.key] ?? ""}
          onChange={(e) => setCustom({ ...custom, [f.key]: e.target.value })}
          help={`{{${f.key}}}`}
        />
      ))}
      <div className="sm:col-span-2">
        <Button busy={save.isPending} onClick={() => save.mutate()}>
          Save
        </Button>
      </div>
    </div>
  );
}

function Notes({ id, notes }: { id: string; notes?: string | null | undefined }) {
  const qc = useQueryClient();
  const [text, setText] = useState("");
  const add = useMutation({
    mutationFn: () => http.post(`/v1/leads/{id}/notes`, { path: { id }, body: { text } }),
    onSuccess: () => {
      setText("");
      void qc.invalidateQueries({ queryKey: ["leads", "context", id] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  return (
    <div className="flex flex-col gap-3">
      <TextArea label="Add a note" value={text} onChange={(e) => setText(e.target.value)} />
      <div>
        <Button disabled={!text.trim()} busy={add.isPending} onClick={() => add.mutate()}>
          Add note
        </Button>
      </div>
      {notes && (
        <pre className="whitespace-pre-wrap rounded-(--radius-control) bg-surface-2 p-3 text-sm">
          {notes}
        </pre>
      )}
    </div>
  );
}
