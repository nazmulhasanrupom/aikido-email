import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { FileUp, Globe, Plus, Sparkles, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S, upload } from "@/api/http";
import {
  Button,
  Card,
  Check2,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Field,
  Notice,
  Page,
  PageHeader,
  Pill,
  Skeleton,
  TextArea,
} from "@/components/ui";
import { fmtAgo } from "@/lib/format";

/** The nine sender fields, in prompt order. */
export const SENDER_FIELDS: { key: Exclude<keyof Fields, "allow_low_personalization">; label: string; help: string }[] = [
  { key: "sender_first_name", label: "Name", help: "Your first name. It signs the email." },
  { key: "ideal_customer", label: "Ideal customer", help: "Who buys: role, company type, size." },
  { key: "not_a_fit", label: "Not a fit", help: "Who should not get your email." },
  { key: "problem_solved", label: "Problem solved", help: "The business problem, in the buyer's words." },
  { key: "offer", label: "Offer", help: "What you do, in one concrete sentence." },
  { key: "outcome", label: "Outcome", help: "The business result the buyer gets. No invented numbers." },
  { key: "proof_or_none", label: "Proof", help: "A real client, number or result. Write none if you have none." },
  { key: "low_friction_offer", label: "Low friction offer", help: "One small next step you can really deliver." },
];

const profileQuery = { queryKey: ["personalization", "profile"], queryFn: () => http.get<S["ProfileReply"]>("/v1/personalization/profile") };
const sourcesQuery = { queryKey: ["personalization", "sources"], queryFn: () => http.get<S["PersonalizationSource"][]>("/v1/personalization/sources") };
const versionsQuery = { queryKey: ["personalization", "versions"], queryFn: () => http.get<S["SenderProfile"][]>("/v1/personalization/profile/versions") };

type Fields = Required<S["SenderFields"]>;
const BLANK: Fields = {
  sender_first_name: "",
  ideal_customer: "",
  not_a_fit: "",
  problem_solved: "",
  offer: "",
  outcome: "",
  proof_or_none: "none",
  low_friction_offer: "",
  allow_low_personalization: false,
};

function fieldsOf(p: S["SenderProfile"] | null | undefined): Fields {
  if (!p) return BLANK;
  return {
    sender_first_name: p.sender_first_name,
    ideal_customer: p.ideal_customer,
    not_a_fit: p.not_a_fit,
    problem_solved: p.problem_solved,
    offer: p.offer,
    outcome: p.outcome,
    proof_or_none: p.proof_or_none,
    low_friction_offer: p.low_friction_offer,
    allow_low_personalization: p.allow_low_personalization,
  };
}

function useRefresh() {
  const qc = useQueryClient();
  return () => qc.invalidateQueries({ queryKey: ["personalization"] });
}

export function PersonalizationPage({ ws }: { ws: string }) {
  const profile = useQuery(profileQuery);
  const sources = useQuery(sourcesQuery);
  const versions = useQuery(versionsQuery);
  const refresh = useRefresh();
  const [draft, setDraft] = useState<S["GenerateReply"] | null>(null);
  const [fields, setFields] = useState<Fields>(BLANK);
  const [wipe, setWipe] = useState(false);
  const saved = profile.data?.profile ?? null;

  // the editor starts from the saved profile, or from the draft the AI made
  useEffect(() => {
    if (!draft) setFields(fieldsOf(saved));
  }, [saved, draft]);

  const generate = useMutation({
    mutationFn: () => http.post<S["GenerateReply"]>("/v1/personalization/profile/generate"),
    onSuccess: (d) => {
      setDraft(d);
      // keep the switch the person set; everything else comes from the AI
      setFields({ ...BLANK, ...d.draft.fields, allow_low_personalization: fields.allow_low_personalization });
      toast.success("Draft ready. Read it, fix it, then press Save.");
    },
  });
  const save = useMutation({
    mutationFn: () =>
      http.put<S["SenderProfile"]>("/v1/personalization/profile", {
        body: {
          fields,
          other_offers: draft?.draft.other_offers ?? [],
          sources_used: draft?.draft.sources_used ?? {},
        },
      }),
    onSuccess: async () => {
      setDraft(null);
      await refresh();
      toast.success("Saved. New emails use this profile.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const noProvider = generate.error instanceof ApiProblem && generate.error.code === "generator_not_ready";
  const cited = (draft?.draft.sources_used ?? {}) as Record<string, string[]>;
  const needsInput = (k: string, v: string) => (k === "proof_or_none" ? false : !v.trim());
  const dirty = JSON.stringify(fields) !== JSON.stringify(fieldsOf(saved));

  return (
    <Page>
      <PageHeader
        title="Personalisation"
        sub="Tell us who you are and what you offer. We write a sender profile that every optimized email uses."
        actions={
          <Button variant="secondary" onClick={() => setWipe(true)} disabled={!saved && !sources.data?.length}>
            <Trash2 size={14} /> Delete all
          </Button>
        }
      />
      {profile.isError && <ErrorNote error={profile.error} onRetry={() => void profile.refetch()} />}
      {!saved && !profile.isPending && (
        <Notice tone="warning">
          No profile is saved yet. Until you save one, optimized emails are held.
        </Notice>
      )}

      <div className="mt-4 grid gap-6 lg:grid-cols-2">
        <section aria-label="Sources" className="flex flex-col gap-4">
          <h2 className="text-base font-medium">1. What we can read</h2>
          <p className="text-sm text-muted">
            Everything is cleaned first. The AI reads only the cleaned text below, never the raw page or the CV file.
          </p>
          <UrlCard />
          <CvCard />
          <TextCard />
          <DetailsCard />
          <GuideCard />
          <CleanedList sources={sources.data} loading={sources.isPending} />
        </section>

        <section aria-label="Sender profile" className="flex flex-col gap-4">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-base font-medium">2. Your sender profile</h2>
            <div className="flex gap-2">
              <Button
                variant="secondary"
                busy={generate.isPending}
                disabled={!sources.data?.length}
                onClick={() => generate.mutate()}
              >
                <Sparkles size={14} /> {draft ? "Regenerate" : "Build with AI"}
              </Button>
            </div>
          </div>
          {noProvider && (
            <Notice tone="warning">
              Set up an AI provider first.{" "}
              <Link to="/w/$ws/settings/integrations/$id" params={{ ws, id: "ai" }} className="underline">
                Open AI models
              </Link>
            </Notice>
          )}
          {generate.isError && !noProvider && <ErrorNote error={generate.error} />}
          {draft && <Notice tone="info">This is a draft from the AI. Nothing is saved until you press Save.</Notice>}

          {SENDER_FIELDS.map((f) => {
            const empty = needsInput(f.key, fields[f.key]);
            return (
              <div key={f.key} className="flex flex-col gap-1">
                <TextArea
                  label={f.label}
                  rows={2}
                  value={fields[f.key]}
                  maxLength={1000}
                  help={f.help}
                  onChange={(e) => setFields({ ...fields, [f.key]: e.target.value })}
                />
                <div className="flex flex-wrap items-center gap-2 text-xs">
                  {empty && <Pill tone="warning">Needs your input</Pill>}
                  {(cited[f.key] ?? []).map((t) => (
                    <Pill key={t} tone="info">
                      {t}
                    </Pill>
                  ))}
                  {saved && fields[f.key] !== fieldsOf(saved)[f.key] && (
                    <button
                      type="button"
                      className="underline"
                      onClick={() => setFields({ ...fields, [f.key]: fieldsOf(saved)[f.key] })}
                    >
                      Reset field
                    </button>
                  )}
                </div>
              </div>
            );
          })}
          {(draft?.draft.other_offers.length ?? 0) > 0 && (
            <p className="text-sm text-muted">Other offers the AI found: {draft?.draft.other_offers.join("; ")}</p>
          )}
          <Check2
            label="Allow low personalization"
            help="Send even when a lead has no note and no checked trigger. Only you can turn this on."
            checked={fields.allow_low_personalization}
            onChange={(v) => setFields({ ...fields, allow_low_personalization: v })}
          />
          <div className="flex gap-2">
            <Button busy={save.isPending} disabled={!dirty && !draft} onClick={() => save.mutate()}>
              Save
            </Button>
            {draft && (
              <Button variant="secondary" onClick={() => setDraft(null)}>
                Discard draft
              </Button>
            )}
          </div>
          <Versions versions={versions.data} />
        </section>
      </div>
      <ConfirmDialog
        open={wipe}
        onClose={() => setWipe(false)}
        title="Delete everything?"
        text="This deletes every source, your CV text and every saved profile version. Optimized emails will be held until you save a new profile."
        confirmLabel="Delete all"
        onConfirm={async () => {
          await http.delEmpty("/v1/personalization");
          setDraft(null);
          await refresh();
          toast.success("Deleted.");
        }}
      />
    </Page>
  );
}

function Versions({ versions }: { versions: S["SenderProfile"][] | undefined }) {
  if (!versions?.length) return null;
  return (
    <details className="text-sm">
      <summary className="cursor-pointer font-medium">Saved versions ({versions.length})</summary>
      <ul className="mt-2 flex flex-col gap-1">
        {versions.map((v) => (
          <li key={v.id} className="flex justify-between gap-3 border-b border-border pb-1">
            <span>
              Version {v.version}
              {v.offer ? `: ${v.offer.slice(0, 60)}` : ""}
            </span>
            <span className="text-muted">{fmtAgo(v.created_at)}</span>
          </li>
        ))}
      </ul>
    </details>
  );
}

function CleanedList({ sources, loading }: { sources: S["PersonalizationSource"][] | undefined; loading: boolean }) {
  const refresh = useRefresh();
  const del = useMutation({
    mutationFn: (id: string) => http.delEmpty("/v1/personalization/sources/{id}", { path: { id } }),
    onSuccess: () => refresh(),
    onError: (e) => toast.error(messageOf(e)),
  });
  if (loading) return <Skeleton rows={2} />;
  if (!sources?.length) return <EmptyState title="Nothing added yet" text="Add a link, a CV, some text or answer the guide questions." />;
  return (
    <Card>
      <h3 className="mb-2 text-sm font-medium">Cleaned text the AI will read</h3>
      <ul className="flex flex-col gap-2">
        {sources.map((s) => (
          <li key={s.id} className="rounded-(--radius-control) border border-border p-2 text-sm">
            <div className="mb-1 flex items-center justify-between gap-2">
              <span className="flex items-center gap-2">
                <Pill tone="info">{s.tag}</Pill>
                <span className="text-xs text-muted">{s.token_count} tokens</span>
              </span>
              <button
                type="button"
                aria-label={`Delete ${s.tag}`}
                className="rounded p-1 hover:bg-surface-2"
                onClick={() => del.mutate(s.id)}
              >
                <Trash2 size={14} />
              </button>
            </div>
            <p className="line-clamp-3 whitespace-pre-wrap text-muted">{s.clean_text}</p>
          </li>
        ))}
      </ul>
    </Card>
  );
}

function useAdd(onDone?: () => void) {
  const refresh = useRefresh();
  return useMutation({
    mutationFn: (body: Record<string, unknown>) => http.post<S["PersonalizationSource"][]>("/v1/personalization/sources", { body }),
    onSuccess: async (rows) => {
      await refresh();
      toast.success(rows.length === 1 ? "Added." : `Added ${rows.length} items.`);
      onDone?.();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
}

function UrlCard() {
  const [url, setUrl] = useState("");
  const add = useAdd(() => setUrl(""));
  return (
    <Card>
      <h3 className="mb-2 flex items-center gap-2 text-sm font-medium">
        <Globe size={14} /> Portfolio link
      </h3>
      <div className="flex items-end gap-2">
        <div className="flex-1">
          <Field label="Address" placeholder="https://yoursite.com" value={url} onChange={(e) => setUrl(e.target.value)} help="Up to 3 sites. We also read up to 4 pages like About or Services." />
        </div>
        <Button busy={add.isPending} disabled={!url.trim()} onClick={() => add.mutate({ kind: "url", url })}>
          Read
        </Button>
      </div>
    </Card>
  );
}

function CvCard() {
  const refresh = useRefresh();
  const input = useRef<HTMLInputElement>(null);
  const up = useMutation({
    mutationFn: (file: File) => {
      const form = new FormData();
      form.append("file", file);
      return upload<S["PersonalizationSource"]>("/v1/personalization/sources/cv", form);
    },
    onSuccess: async () => {
      await refresh();
      toast.success("CV text added. The file itself is not kept.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  return (
    <Card>
      <h3 className="mb-2 flex items-center gap-2 text-sm font-medium">
        <FileUp size={14} /> CV
      </h3>
      <input
        ref={input}
        type="file"
        aria-label="CV file"
        accept=".pdf,.docx,.txt"
        className="text-sm"
        onChange={(e) => {
          const f = e.target.files?.[0];
          if (f) up.mutate(f);
          e.target.value = "";
        }}
      />
      <p className="mt-1 text-xs text-muted">PDF, DOCX or TXT, up to 5 MB. We keep only the text. A CV is personal data: you can delete it any time.</p>
    </Card>
  );
}

function TextCard() {
  const [text, setText] = useState("");
  const add = useAdd(() => setText(""));
  return (
    <Card>
      <TextArea
        label="Tell us about you and your offer"
        rows={4}
        maxLength={10000}
        value={text}
        onChange={(e) => setText(e.target.value)}
        help={`${text.length} of 10000 characters`}
      />
      <div className="mt-2">
        <Button busy={add.isPending} disabled={!text.trim()} onClick={() => add.mutate({ kind: "text", text })}>
          Add text
        </Button>
      </div>
    </Card>
  );
}

function DetailsCard() {
  const [rows, setRows] = useState<{ key: string; value: string }[]>([{ key: "", value: "" }]);
  const add = useAdd(() => setRows([{ key: "", value: "" }]));
  return (
    <Card>
      <h3 className="mb-2 text-sm font-medium">Extra details</h3>
      <div className="flex flex-col gap-2">
        {rows.map((r, i) => (
          // biome-ignore lint/suspicious/noArrayIndexKey: rows have no id
          <div key={i} className="flex gap-2">
            <Field label="Detail name" placeholder="Clients" value={r.key} onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)))} />
            <div className="flex-1">
              <Field label="Detail value" placeholder="Smile Co, Acme" value={r.value} onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, value: e.target.value } : x)))} />
            </div>
          </div>
        ))}
      </div>
      <div className="mt-2 flex gap-2">
        <Button variant="secondary" disabled={rows.length >= 20} onClick={() => setRows([...rows, { key: "", value: "" }])}>
          <Plus size={14} /> Add row
        </Button>
        <Button busy={add.isPending} disabled={!rows.some((r) => r.key.trim() && r.value.trim())} onClick={() => add.mutate({ kind: "detail", details: rows })}>
          Save details
        </Button>
      </div>
    </Card>
  );
}

function GuideCard() {
  const [g, setG] = useState({ who_you_help: "", who_you_do_not_help: "", result_you_give: "", real_proof: "" });
  const add = useAdd();
  const set = (k: keyof typeof g) => (e: React.ChangeEvent<HTMLTextAreaElement>) => setG({ ...g, [k]: e.target.value });
  return (
    <Card>
      <h3 className="mb-2 text-sm font-medium">Guide questions</h3>
      <div className="flex flex-col gap-2">
        <TextArea label="Who do you help?" rows={2} value={g.who_you_help} onChange={set("who_you_help")} />
        <TextArea label="Who do you not help?" rows={2} value={g.who_you_do_not_help} onChange={set("who_you_do_not_help")} />
        <TextArea label="What result do you give?" rows={2} value={g.result_you_give} onChange={set("result_you_give")} />
        <TextArea label="What real proof do you have?" rows={2} value={g.real_proof} onChange={set("real_proof")} help="Real clients, numbers or links only." />
      </div>
      <div className="mt-2">
        <Button busy={add.isPending} disabled={!Object.values(g).some((v) => v.trim())} onClick={() => add.mutate({ kind: "guide", guide: g })}>
          Save answers
        </Button>
      </div>
    </Card>
  );
}
