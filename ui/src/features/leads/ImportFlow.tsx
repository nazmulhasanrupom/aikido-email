import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S, upload } from "@/api/http";
import { q } from "@/api/queries";
import { Button, Check2, ErrorNote, Modal, Notice, Pill, Select } from "@/components/ui";
import { cn } from "@/lib/cn";
import { fmtInt } from "@/lib/format";
import { say } from "@/lib/words";

type Import = S["Import"];
type Proposal = S["MappingProposal"];
type Target = S["MapTarget"];

const BUILTINS = [
  "email",
  "first_name",
  "last_name",
  "company_name",
  "job_title",
  "phone",
  "website",
  "linkedin_url",
  "location",
  "timezone",
  "notes",
  "tags",
];

const DEDUPE: { value: S["DedupeMode"]; label: string; help: string }[] = [
  {
    value: "fill_empty",
    label: "Fill empty fields",
    help: "Keep what we have. Add only what is missing.",
  },
  { value: "skip", label: "Skip", help: "Leave leads we already have as they are." },
  { value: "overwrite", label: "Overwrite", help: "Replace our values with the ones in the file." },
];

/** `kind:key` text for a select, and back. */
function encode(t: Target): string {
  if (t.kind === "skip") return "skip";
  return `${t.kind}:${t.key}`;
}
function decode(v: string, keepLabel?: Target): Target {
  if (v === "skip") return { kind: "skip" };
  const i = v.indexOf(":");
  const kind = v.slice(0, i);
  const key = v.slice(i + 1);
  if (kind === "new_field") {
    return keepLabel?.kind === "new_field"
      ? { ...keepLabel, key }
      : { kind: "new_field", key, field_kind: "text" };
  }
  return { kind: kind as "builtin" | "custom", key };
}

/**
 * Upload a CSV, look at the proposed column mapping, confirm, watch the progress.
 * Used by onboarding, the Leads page and a campaign.
 */
export function ImportFlow({
  compact,
  campaignId,
  listId,
  onFinished,
}: {
  compact?: boolean;
  campaignId?: string | undefined;
  listId?: string | undefined;
  onFinished?: (imp: Import) => void;
}) {
  const qc = useQueryClient();
  const settings = useQuery(q.leadSettings);
  const lists = useQuery(q.lists);
  const input = useRef<HTMLInputElement>(null);
  const [dedupe, setDedupe] = useState<S["DedupeMode"]>("fill_empty");
  const [list, setList] = useState(listId ?? "");
  const [tags, setTags] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [over, setOver] = useState(false);
  const [phase, setPhase] = useState<"pick" | "reading" | "map" | "run" | "done">("pick");
  const [error, setError] = useState<unknown>(null);
  const [imp, setImp] = useState<Import | null>(null);
  const [proposal, setProposal] = useState<Proposal | null>(null);

  useEffect(() => {
    if (settings.data) setDedupe(settings.data.settings.default_dedupe_mode);
  }, [settings.data]);

  async function startUpload() {
    if (!file) return;
    setPhase("reading");
    setError(null);
    try {
      const form = new FormData();
      form.append("dedupe_mode", dedupe);
      if (list) form.append("list_id", list);
      if (tags.trim()) form.append("tags", tags.trim());
      if (campaignId) form.append("campaign_id", campaignId);
      form.append("file", file);
      const res = await upload<Import & { proposal?: Proposal }>("/v1/leads/imports", form, {
        auto_map: true,
      });
      setImp(res);
      if (res.proposal) {
        setProposal(res.proposal);
        setPhase("map");
      } else {
        setPhase("run");
      }
    } catch (e) {
      setError(e);
      setPhase("pick");
    }
  }

  const maxMb = settings.data
    ? Math.round(settings.data.settings.import_max_bytes / 1_048_576)
    : 50;
  const maxRows = settings.data?.settings.import_max_rows ?? 100000;

  return (
    <div className="flex flex-col gap-4">
      {phase === "pick" || phase === "reading" ? (
        <>
          <button
            type="button"
            onClick={() => input.current?.click()}
            onDragOver={(e) => {
              e.preventDefault();
              setOver(true);
            }}
            onDragLeave={() => setOver(false)}
            onDrop={(e) => {
              e.preventDefault();
              setOver(false);
              const f = e.dataTransfer.files[0];
              if (f) setFile(f);
            }}
            className={cn(
              "flex flex-col items-center gap-1 rounded-(--radius-card) border-2 border-dashed px-6 py-8 text-center",
              over ? "border-accent bg-accent-soft" : "border-border hover:bg-surface-2",
            )}
          >
            <span className="font-medium">
              {file ? file.name : "Drop a CSV file here, or click to choose one"}
            </span>
            <span className="text-xs text-muted">
              Up to {maxMb} MB and {fmtInt(maxRows)} rows.
            </span>
          </button>
          <input
            ref={input}
            type="file"
            accept=".csv,text/csv"
            aria-label="CSV file"
            className="sr-only"
            onChange={(e) => setFile(e.target.files?.[0] ?? null)}
          />
          {!compact && (
            <div className="grid gap-3 sm:grid-cols-2">
              <Select
                label="If we already have the lead"
                value={dedupe}
                options={DEDUPE.map((d) => ({ value: d.value, label: d.label }))}
                onChange={(v) => setDedupe(v as S["DedupeMode"])}
                help={DEDUPE.find((d) => d.value === dedupe)?.help}
              />
              <Select
                label="Add to list"
                value={list}
                options={[
                  { value: "", label: "No list" },
                  ...(lists.data ?? []).map((l) => ({ value: l.id, label: l.name })),
                ]}
                onChange={setList}
              />
              <label className="flex flex-col gap-1.5 text-sm sm:col-span-2">
                <span className="font-medium">Tags (optional)</span>
                <input
                  value={tags}
                  onChange={(e) => setTags(e.target.value)}
                  placeholder="webinar, 2026"
                  className="h-9 rounded-(--radius-control) border border-border bg-surface-2 px-3"
                />
              </label>
            </div>
          )}
          {error != null && (
            <Notice tone="danger">
              {error instanceof ApiProblem && error.status === 413
                ? "The file is too big."
                : messageOf(error)}
            </Notice>
          )}
          <div>
            <Button onClick={startUpload} disabled={!file} busy={phase === "reading"}>
              {phase === "reading" ? "Reading columns..." : "Upload and map columns"}
            </Button>
          </div>
        </>
      ) : null}

      {phase === "map" && imp && proposal && (
        <MappingDialog
          imp={imp}
          proposal={proposal}
          onProposal={setProposal}
          listId={list}
          campaignId={campaignId}
          dedupe={dedupe}
          tags={tags}
          onCancel={async () => {
            await http
              .post("/v1/leads/imports/{id}/cancel", { path: { id: imp.id } })
              .catch(() => {});
            setPhase("pick");
            setFile(null);
          }}
          onConfirmed={(next) => {
            setImp(next);
            setPhase("run");
          }}
        />
      )}

      {phase === "run" && imp && (
        <Progress
          id={imp.id}
          onDone={(final) => {
            setImp(final);
            setPhase("done");
            void qc.invalidateQueries({ queryKey: ["leads"] });
            onFinished?.(final);
          }}
        />
      )}

      {phase === "done" && imp && (
        <div className="flex flex-col gap-3">
          <Summary imp={imp} />
          <div>
            <Button
              variant="secondary"
              onClick={() => {
                setPhase("pick");
                setFile(null);
                setImp(null);
              }}
            >
              Import another file
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

export function Summary({ imp }: { imp: Import }) {
  const failed = imp.status === "failed" || imp.status === "cancelled";
  return (
    <div className="flex flex-col gap-2">
      <Notice tone={failed ? "danger" : "success"}>
        {failed ? `${say("importStatus", imp.status)}. ${imp.error ?? ""}` : "The import is done."}
      </Notice>
      <div className="flex flex-wrap gap-2">
        <Pill tone="success">{fmtInt(imp.created)} new</Pill>
        <Pill tone="info">{fmtInt(imp.updated)} updated</Pill>
        <Pill>{fmtInt(imp.skipped)} skipped</Pill>
        <Pill tone={imp.invalid ? "warning" : "neutral"}>{fmtInt(imp.invalid)} not valid</Pill>
      </div>
    </div>
  );
}

function Progress({ id, onDone }: { id: string; onDone: (i: Import) => void }) {
  const res = useQuery({
    queryKey: ["leads", "imports", id],
    queryFn: () => http.get<Import>("/v1/leads/imports/{id}", { path: { id } }),
    refetchInterval: (query) => {
      const s = query.state.data?.status;
      return s === "done" || s === "failed" || s === "cancelled" ? false : 2000;
    },
  });
  const d = res.data;
  const finished = d && (d.status === "done" || d.status === "failed" || d.status === "cancelled");
  useEffect(() => {
    if (finished && d) onDone(d);
  }, [finished]);
  if (res.isError) return <ErrorNote error={res.error} />;
  const pct =
    d && d.total_rows > 0 ? Math.min(100, Math.round((d.rows_done / d.total_rows) * 100)) : 0;
  return (
    <div className="flex flex-col gap-2" aria-live="polite">
      <div className="flex justify-between text-sm">
        <span>{d ? say("importStatus", d.status) : "Starting..."}</span>
        <span className="text-muted">
          {fmtInt(d?.rows_done)} of {fmtInt(d?.total_rows)} rows
        </span>
      </div>
      <div
        className="h-2 overflow-hidden rounded-full bg-surface-2"
        role="progressbar"
        aria-valuenow={pct}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <div className="h-full bg-accent transition-all" style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

function MappingDialog({
  imp,
  proposal,
  onProposal,
  listId,
  campaignId,
  dedupe,
  tags,
  onCancel,
  onConfirmed,
}: {
  imp: Import;
  proposal: Proposal;
  onProposal: (p: Proposal) => void;
  listId: string;
  campaignId: string | undefined;
  dedupe: S["DedupeMode"];
  tags: string;
  onCancel: () => void;
  onConfirmed: (i: Import) => void;
}) {
  const fields = useQuery(q.fields);
  const [cols, setCols] = useState(proposal.columns);
  const [changed, setChanged] = useState<Set<number>>(new Set());
  const [remember, setRemember] = useState(true);
  const [raw, setRaw] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problems, setProblems] = useState<string[]>([]);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    setCols(proposal.columns);
    setChanged(new Set());
  }, [proposal]);

  const options = (c: Proposal["columns"][number]) => {
    const out = [
      ...BUILTINS.map((k) => ({
        value: `builtin:${k}`,
        label: k === "email" ? "Email" : k.replaceAll("_", " "),
      })),
      ...(fields.data ?? []).map((f) => ({
        value: `custom:${f.key}`,
        label: `Field: ${f.label || f.key}`,
      })),
    ];
    const t = c.target;
    if (t.kind === "new_field") out.push({ value: encode(t), label: `+ New field: ${t.key}` });
    out.push({ value: "skip", label: "Skip this column" });
    return out;
  };

  function setTarget(i: number, v: string) {
    setCols((cs) =>
      cs.map((c) =>
        c.index === i
          ? {
              ...c,
              target: decode(v, c.target),
              transform: v === "builtin:first_name" ? c.transform : null,
            }
          : c,
      ),
    );
    setChanged((s) => new Set(s).add(i));
  }
  function setSplit(i: number, on: boolean) {
    setCols((cs) =>
      cs.map((c) => (c.index === i ? { ...c, transform: on ? "split_full_name" : null } : c)),
    );
    setChanged((s) => new Set(s).add(i));
  }

  async function confirm() {
    setBusy(true);
    setError(null);
    setProblems([]);
    try {
      const columns = cols
        .filter((c) => changed.has(c.index))
        .map((c) => ({ index: c.index, target: c.target, transform: c.transform ?? null }));
      const body: S["ConfirmRequest"] = {
        columns,
        remember,
        dedupe_mode: dedupe,
        ...(listId ? { list_id: listId } : {}),
        ...(campaignId ? { campaign_id: campaignId } : {}),
        ...(tags.trim()
          ? {
              tags: tags
                .split(",")
                .map((t) => t.trim())
                .filter(Boolean),
            }
          : {}),
      };
      onConfirmed(
        await http.post<Import>("/v1/leads/imports/{id}/mapping/confirm", {
          path: { id: imp.id },
          body,
        }),
      );
    } catch (e) {
      if (e instanceof ApiProblem && e.code === "mapping_invalid")
        setProblems((e.details.problems as string[]) ?? [e.message]);
      else setError(e);
    } finally {
      setBusy(false);
    }
  }

  async function again() {
    setBusy(true);
    try {
      onProposal(
        await http.post<Proposal>("/v1/leads/imports/{id}/mapping/propose", {
          path: { id: imp.id },
        }),
      );
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  async function showRaw() {
    const next = !raw;
    setRaw(next);
    try {
      const p = await http.get<Proposal>("/v1/leads/imports/{id}/mapping", {
        path: { id: imp.id },
        query: { raw_samples: next },
      });
      setCols((cs) =>
        cs.map((c) => ({
          ...c,
          samples: p.columns.find((x) => x.index === c.index)?.samples ?? c.samples,
        })),
      );
    } catch (e) {
      setError(e);
    }
  }

  return (
    <Modal
      open
      onClose={onCancel}
      size="xl"
      title="Match the columns of your file"
      footer={
        <>
          <Button variant="secondary" onClick={onCancel}>
            Cancel import
          </Button>
          <Button onClick={confirm} busy={busy}>
            Import
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {proposal.warnings.map((w) => (
          <Notice key={w} tone="warning">
            {w}
          </Notice>
        ))}
        {problems.length > 0 && (
          <Notice tone="danger">
            <ul className="list-disc pl-4">
              {problems.map((p) => (
                <li key={p}>{p}</li>
              ))}
            </ul>
          </Notice>
        )}
        {error != null && <ErrorNote error={error} />}
        <div className="overflow-x-auto rounded-(--radius-card) border border-border">
          <table className="w-full text-[13px]">
            <thead className="bg-surface-2 text-left text-xs text-muted">
              <tr>
                <th className="px-3 py-2">Column in your file</th>
                <th className="px-3 py-2">Example values</th>
                <th className="px-3 py-2">Goes to</th>
                <th className="px-3 py-2">How</th>
              </tr>
            </thead>
            <tbody>
              {cols.map((c) => {
                const check = proposal.needs_check.includes(c.index);
                return (
                  <tr
                    key={c.index}
                    className={cn("border-t border-border align-top", check && "bg-warning/5")}
                  >
                    <td className="px-3 py-2 font-medium">
                      {c.header || `(column ${c.index + 1})`}
                    </td>
                    <td className="max-w-56 px-3 py-2 text-muted">
                      {c.samples.slice(0, 3).map((s) => (
                        <div key={s} className="truncate">
                          {s}
                        </div>
                      ))}
                    </td>
                    <td className="px-3 py-2">
                      <Select
                        label={`Target for ${c.header}`}
                        hideLabel
                        value={encode(c.target)}
                        options={options(c)}
                        onChange={(v) => setTarget(c.index, v)}
                      />
                      {c.target.kind === "builtin" && c.target.key === "first_name" && (
                        <div className="mt-2">
                          <Check2
                            label="Split full name"
                            checked={c.transform === "split_full_name"}
                            onChange={(on) => setSplit(c.index, on)}
                          />
                        </div>
                      )}
                    </td>
                    <td className="px-3 py-2">
                      <div className="flex flex-wrap gap-1">
                        <Pill tone={c.source === "ai" ? "ai" : "neutral"}>
                          {c.source === "ai" ? "AI" : c.source === "template" ? "Saved" : "Rule"}
                        </Pill>
                        {check && <Pill tone="warning">Check this</Pill>}
                      </div>
                      <p className="mt-1 text-xs text-muted">{c.reason}</p>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        <div className="flex flex-wrap items-center gap-4">
          <Check2
            label="Remember for files with these columns"
            checked={remember}
            onChange={setRemember}
          />
          <Button variant="secondary" className="h-8" onClick={again} disabled={busy}>
            Map again
          </Button>
          <Button variant="ghost" className="h-8" onClick={showRaw}>
            {raw ? "Hide real values" : "Show real values"}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
