import { useQuery } from "@tanstack/react-query";
import { Radio, X } from "lucide-react";
import { useEffect, useState } from "react";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { Button, CopyButton, useDebounced } from "@/components/ui";
import { call } from "./api";
import { CONTROL, JsonField } from "./ParamForm";
import { needsLead, type TestInput } from "./testInput";
import type { WfNode } from "./types";

export interface Listening {
  nodeId: string;
  until: string;
  url: string;
}

function useNow(active: boolean): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(t);
  }, [active]);
  return now;
}

/** The test event picker of a trigger node (map 10.2), and the URLs and listen button of a webhook. */
export function TriggerTools({
  node,
  input,
  onInput,
  listening,
  onListen,
  readOnly,
}: {
  node: WfNode;
  input: TestInput | undefined;
  onInput: (patch: TestInput) => void;
  listening: Listening | null;
  onListen: () => void;
  readOnly: boolean;
}) {
  const i = input ?? {};
  const type = node.type;
  const noTools = type === "trigger.schedule" || type === "trigger.cron";
  return (
    <section className="flex flex-col gap-3 rounded-(--radius-control) border border-border bg-surface-2/40 p-3" aria-label="Test event">
      <h3 className="text-sm font-semibold">Test event</h3>
      {noTools && <p className="text-xs text-muted">A test starts now: the time of the run is the time you press the button.</p>}
      {needsLead(type) && <LeadPicker value={i} onPick={onInput} />}
      {(needsLead(type) || type === "trigger.reply_received") && <p className="text-xs text-muted">Pick a real one, or leave it empty: the test then uses an example.</p>}
      {type === "trigger.lead_added_to_campaign" && <CampaignPick value={i.campaignId} onChange={(campaignId) => onInput({ campaignId })} />}
      {type === "trigger.lead_added_to_list" && <ListPick value={i.listId} onChange={(listId) => onInput({ listId })} />}
      {type === "trigger.reply_received" && <MessagePicker value={i} onPick={onInput} />}
      {type === "trigger.manual" && (
        <div className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Input (JSON, optional)</span>
          <JsonField label="Input" value={parseOrUndefined(i.json)} onChange={(v) => onInput({ json: v === undefined ? "" : JSON.stringify(v) })} rows={3} />
          <span className="text-xs text-muted">It becomes {"${{ trigger.input }}"}.</span>
        </div>
      )}
      {type === "trigger.webhook" && <Webhook node={node} input={i} onInput={onInput} listening={listening} onListen={onListen} readOnly={readOnly} />}
    </section>
  );
}

function parseOrUndefined(text: string | undefined): unknown {
  if (!text?.trim()) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

function Webhook({
  node,
  input,
  onInput,
  listening,
  onListen,
  readOnly,
}: {
  node: WfNode;
  input: TestInput;
  onInput: (patch: TestInput) => void;
  listening: Listening | null;
  onListen: () => void;
  readOnly: boolean;
}) {
  const path = String(node.params?.path ?? "");
  const origin = window.location.origin;
  const mine = listening?.nodeId === node.id ? listening : null;
  const now = useNow(!!mine);
  const left = mine ? Math.max(0, Math.round((new Date(mine.until).getTime() - now) / 1000)) : 0;
  return (
    <div className="flex flex-col gap-3">
      {path ? (
        <div className="flex flex-col gap-1.5 text-xs">
          <div className="flex items-center justify-between gap-2">
            <span className="min-w-0 truncate font-mono" title={`${origin}/hooks/test/${path}`}>Test: {origin}/hooks/test/{path}</span>
            <CopyButton text={`${origin}/hooks/test/${path}`} />
          </div>
          <div className="flex items-center justify-between gap-2">
            <span className="min-w-0 truncate font-mono" title={`${origin}/hooks/${path}`}>Production: {origin}/hooks/{path}</span>
            <CopyButton text={`${origin}/hooks/${path}`} />
          </div>
          <p className="text-muted">The production URL works only while the workflow is active.</p>
        </div>
      ) : (
        <p className="text-xs text-warning">Set a path in the settings below. The URLs are made from it.</p>
      )}
      <Button variant="secondary" disabled={readOnly || !path || !!mine} onClick={onListen}>
        <Radio size={15} aria-hidden="true" /> {mine ? `Listening… ${left}s` : "Listen for test event"}
      </Button>
      {mine && <p className="text-xs text-muted">Send one request to the test URL. The run starts by itself.</p>}
      <div className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Or test with a body (JSON)</span>
        <JsonField label="Body" value={parseOrUndefined(input.json)} onChange={(v) => onInput({ json: v === undefined ? "" : JSON.stringify(v) })} rows={3} />
      </div>
    </div>
  );
}

function LeadPicker({ value, onPick }: { value: TestInput; onPick: (p: TestInput) => void }) {
  const [text, setText] = useState("");
  const term = useDebounced(text.trim(), 250);
  const found = useQuery({
    queryKey: ["automations", "lead-search", term],
    queryFn: () => http.get<S["ListLeadsReply"]>("/v1/leads", { query: { q: term, limit: 8 } }),
    enabled: term.length > 0,
  });
  return (
    <div className="flex flex-col gap-1.5">
      <span className="text-sm font-medium">Lead</span>
      {value.leadId ? (
        <div className="flex items-center justify-between rounded-(--radius-control) border border-border bg-surface px-3 py-1.5 text-sm">
          <span className="truncate">{value.leadLabel}</span>
          <button type="button" aria-label="Pick another lead" onClick={() => onPick({ leadId: "", leadLabel: "" })} className="text-muted hover:text-text">
            <X size={14} />
          </button>
        </div>
      ) : (
        <>
          <input aria-label="Search leads" value={text} onChange={(e) => setText(e.target.value)} placeholder="Search by email or name" className={CONTROL} />
          {found.data && (
            <ul className="max-h-48 overflow-auto rounded-(--radius-control) border border-border bg-surface">
              {found.data.leads.map((l) => (
                <li key={l.id}>
                  <button
                    type="button"
                    onClick={() => onPick({ leadId: l.id, leadLabel: [l.first_name, l.last_name].filter(Boolean).join(" ") ? `${[l.first_name, l.last_name].filter(Boolean).join(" ")} <${l.email}>` : l.email })}
                    className="block w-full truncate px-3 py-1.5 text-left text-sm hover:bg-surface-2"
                  >
                    {l.email}
                    {l.company_name ? <span className="text-muted"> · {l.company_name}</span> : null}
                  </button>
                </li>
              ))}
              {found.data.leads.length === 0 && <li className="px-3 py-2 text-sm text-muted">No lead found.</li>}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

function CampaignPick({ value, onChange }: { value: string | undefined; onChange: (v: string) => void }) {
  const campaigns = useQuery(q.campaigns);
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium">Campaign</span>
      <select value={value ?? ""} onChange={(e) => onChange(e.target.value)} className={CONTROL}>
        <option value="">Any (the first the lead is in)</option>
        {(campaigns.data?.campaigns ?? []).map((c) => (
          <option key={c.id} value={c.id}>
            {c.name}
          </option>
        ))}
      </select>
    </label>
  );
}

function ListPick({ value, onChange }: { value: string | undefined; onChange: (v: string) => void }) {
  const lists = useQuery(q.lists);
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium">List</span>
      <select value={value ?? ""} onChange={(e) => onChange(e.target.value)} className={CONTROL}>
        <option value="">Any</option>
        {(lists.data ?? []).map((l) => (
          <option key={l.id} value={l.id}>
            {l.name}
          </option>
        ))}
      </select>
    </label>
  );
}

function MessagePicker({ value, onPick }: { value: TestInput; onPick: (p: TestInput) => void }) {
  const messages = useQuery({
    queryKey: ["automations", "message-pick"],
    queryFn: () => http.get<S["ListMessagesReply"]>("/v1/replies/messages", { query: { limit: 25 } }),
  });
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium">Reply</span>
      <select
        value={value.messageId ?? ""}
        onChange={(e) => {
          const m = messages.data?.messages.find((x) => x.id === e.target.value);
          onPick({ messageId: e.target.value, messageLabel: m ? `${m.from_email}` : "" });
        }}
        className={CONTROL}
      >
        <option value="">Choose a stored message…</option>
        {(messages.data?.messages ?? []).map((m) => (
          <option key={m.id} value={m.id}>
            {m.from_email} · {m.subject ?? "(no subject)"}
          </option>
        ))}
      </select>
    </label>
  );
}

/** The next 5 times of a cron expression (map 7.1), read from the server so the rule is the same as at run time. */
export function CronPreview({ expression, timezone }: { expression: string; timezone: string }) {
  const text = useDebounced(expression.trim(), 400);
  const next = useQuery({
    queryKey: ["automations", "cron", text, timezone],
    queryFn: () => call.cron(text, timezone),
    enabled: text.length > 0 && !text.includes("${{"),
    retry: false,
  });
  if (!text || text.includes("${{")) return null;
  if (next.isPending) return <p className="text-xs text-muted">…</p>;
  if (next.isError) {
    return (
      <p role="status" className="text-xs text-danger">
        {messageOf(next.error)}
      </p>
    );
  }
  return (
    <div role="status" className="text-xs text-muted">
      <p className="mb-0.5 font-medium text-text">Next times ({next.data.timezone})</p>
      <ul>
        {next.data.local.map((t) => (
          <li key={t} className="font-mono">
            {t}
          </li>
        ))}
      </ul>
    </div>
  );
}
