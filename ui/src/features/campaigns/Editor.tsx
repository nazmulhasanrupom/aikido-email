import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Braces, Plus, Sparkles, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import {
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuRoot,
  MenuSeparator,
  MenuTrigger,
} from "@/components/shell/menu";
import { Button, ConfirmDialog, Field, Modal, Notice, Pill, TextArea } from "@/components/ui";
import { cn } from "@/lib/cn";
import { htmlToText, sanitizeHtml, textToHtml } from "@/lib/html";
import { aiKeys, BUILTIN_VARS, variables, withFallback } from "@/lib/template";
import { PROBLEM_WORDS } from "@/lib/words";
import { AiPartDialog } from "./AiPartDialog";
import { type Brief, BriefForm, EMPTY_BRIEF } from "./NewCampaign";
import { PreviewDrawer } from "./PreviewDrawer";

type DelayType = S["DelayType"];
interface Draft {
  id: string;
  subject: string;
  body_text: string;
  body_html: string | null;
  html: boolean;
  delay_amount: number;
  delay_type: DelayType;
  ai_slots: S["AiSlot"][];
}
let counter = 0;
const uid = () => `d${++counter}`;
const fromStep = (s: S["SequenceStep"]): Draft => ({
  id: s.id,
  subject: s.subject,
  body_text: s.body_text ?? "",
  body_html: s.body_html ?? null,
  html: !!s.body_html,
  delay_amount: s.delay_amount,
  delay_type: s.delay_type,
  ai_slots: s.ai_slots,
});
const fromInput = (s: S["StepInput"]): Draft => ({
  id: uid(),
  subject: s.subject ?? "",
  body_text: s.body_text ?? "",
  body_html: s.body_html ?? null,
  html: !!s.body_html,
  delay_amount: s.delay_amount ?? 3,
  delay_type: s.delay_type ?? "business_days",
  ai_slots: s.ai_slots ?? [],
});

const LEAD_VARS = [
  "first_name",
  "last_name",
  "company_name",
  "job_title",
  "website",
  "location",
  "phone",
  "linkedin_url",
];

export function Editor({
  campaign,
  userEmail,
}: {
  campaign: S["CampaignDetail"];
  userEmail: string;
}) {
  const qc = useQueryClient();
  const fields = useQuery(q.fields);
  const gen = useQuery(q.genSettings);
  const providers = useQuery(q.providers);
  const seq = campaign.sequence;
  const thread = seq.thread_followups;
  const [steps, setSteps] = useState<Draft[]>(() => campaign.steps.map(fromStep));
  const [sel, setSel] = useState(0);
  const [save, setSave] = useState<"saved" | "dirty" | "saving" | "error">("saved");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [preview, setPreview] = useState(false);
  const [aiDialog, setAiDialog] = useState<{ slot: S["AiSlot"] | null } | null>(null);
  const [del, setDel] = useState<number | null>(null);
  const [writeAi, setWriteAi] = useState(false);
  const [toText, setToText] = useState(false);
  const [brief, setBrief] = useState(seq.brief ?? "");
  const area = useRef<HTMLTextAreaElement>(null);
  const cursor = useRef<number | null>(null);
  const first = useRef(true);
  const cur = steps[Math.min(sel, steps.length - 1)] ?? steps[0];
  const slotProvider = gen.data?.settings.slot_provider_id;
  const aiReady =
    !!slotProvider && providers.data?.some((p) => p.id === slotProvider && p.status === "ok");

  const body = useCallback((s: Draft) => (s.html ? (s.body_html ?? "") : s.body_text), []);

  // Autosave 1.5 s after the last change.
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    setSave("dirty");
    const t = setTimeout(async () => {
      setSave("saving");
      try {
        const payload = steps.map((s, i) => {
          const html = s.html ? sanitizeHtml(s.body_html ?? "") : null;
          return {
            subject: i > 0 && thread ? "" : s.subject,
            body_text: s.html ? htmlToText(html ?? "") : s.body_text,
            body_html: html,
            delay_amount: i === 0 ? 0 : s.delay_amount,
            delay_type: s.delay_type,
            ai_slots: s.ai_slots,
          };
        });
        await http.put(`/v1/followups/sequences/{id}/steps`, {
          path: { id: seq.id },
          body: { steps: payload },
        });
        setSave("saved");
        setSaveError(null);
        void qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] });
      } catch (e) {
        setSave("error");
        setSaveError(e instanceof ApiProblem ? (PROBLEM_WORDS[e.code] ?? e.message) : messageOf(e));
      }
    }, 1500);
    return () => clearTimeout(t);
  }, [steps]);

  const patch = (p: Partial<Draft>) =>
    setSteps((all) => all.map((s, i) => (i === sel ? { ...s, ...p } : s)));
  const setBody = (v: string) => patch(cur?.html ? { body_html: v } : { body_text: v });

  function insert(text: string) {
    if (!cur) return;
    const b = body(cur);
    const at = cursor.current ?? b.length;
    setBody(b.slice(0, at) + text + b.slice(at));
    cursor.current = at + text.length;
    requestAnimationFrame(() => {
      area.current?.focus();
      area.current?.setSelectionRange(cursor.current ?? 0, cursor.current ?? 0);
    });
  }
  function wrap(open: string, close: string) {
    const el = area.current;
    if (!el || !cur) return;
    const a = el.selectionStart,
      z = el.selectionEnd;
    const b = body(cur);
    setBody(b.slice(0, a) + open + b.slice(a, z) + close + b.slice(z));
  }

  function move(i: number, d: -1 | 1) {
    const j = i + d;
    if (j < 1 || j >= steps.length) return;
    setSteps((all) => {
      const c = [...all];
      [c[i], c[j]] = [c[j] as Draft, c[i] as Draft];
      return c;
    });
    setSel(j);
  }
  function addStep() {
    if (steps.length >= 10) return;
    setSteps([
      ...steps,
      {
        id: uid(),
        subject: "",
        body_text: "Hi {{first_name|there}},\n\n\n\nBest,\n{{sender_name}}",
        body_html: null,
        html: false,
        delay_amount: 3,
        delay_type: "business_days",
        ai_slots: [],
      },
    ]);
    setSel(steps.length);
  }

  const known = useMemo(
    () => new Set([...BUILTIN_VARS, ...LEAD_VARS, ...(fields.data ?? []).map((f) => f.key)]),
    [fields.data],
  );
  const text = cur ? `${cur.subject}\n${body(cur)}` : "";
  const vars = (() => {
    try {
      return variables(text);
    } catch {
      return [];
    }
  })();
  const keys = (() => {
    try {
      return aiKeys(text);
    } catch {
      return [];
    }
  })();
  const parseError = (() => {
    try {
      variables(text);
      return null;
    } catch (e) {
      return (e as Error).message;
    }
  })();

  if (!cur) return null;
  const showSubject = sel === 0 || !thread;
  const live = campaign.status === "active" || campaign.status === "paused";

  return (
    <div className="flex flex-col gap-3">
      {live && (
        <Notice tone="info">
          This campaign is live. Changes apply to emails not written yet. Sent emails do not change.
        </Notice>
      )}
      <div className="grid gap-5 lg:grid-cols-[280px_1fr]">
        {/* steps rail */}
        <aside className="flex flex-col gap-1" aria-label="Steps">
          <div className="mb-1 flex justify-end">
            <Button
              variant="ghost"
              className="h-7"
              disabled={!aiReady}
              title={aiReady ? undefined : "Set up an AI model first"}
              onClick={() => setWriteAi(true)}
            >
              <Sparkles size={14} /> Write with AI
            </Button>
          </div>
          {steps.map((s, i) => (
            <div key={s.id}>
              {i > 0 && (
                <div className="my-1 flex items-center gap-2 pl-3 text-xs text-muted">
                  wait
                  <input
                    aria-label={`Days to wait before follow-up ${i}`}
                    type="number"
                    min={1}
                    max={365}
                    value={s.delay_amount}
                    onChange={(e) =>
                      setSteps((all) =>
                        all.map((x, k) =>
                          k === i ? { ...x, delay_amount: Number(e.target.value) } : x,
                        ),
                      )
                    }
                    className="h-7 w-14 rounded border border-border bg-surface-2 px-1.5 text-text"
                  />
                  <select
                    aria-label={`Kind of days before follow-up ${i}`}
                    value={s.delay_type}
                    onChange={(e) =>
                      setSteps((all) =>
                        all.map((x, k) =>
                          k === i ? { ...x, delay_type: e.target.value as DelayType } : x,
                        ),
                      )
                    }
                    className="h-7 rounded border border-border bg-surface-2 px-1 text-text"
                  >
                    <option value="business_days">business days</option>
                    <option value="calendar_days">calendar days</option>
                  </select>
                </div>
              )}
              <div
                className={cn(
                  "flex items-center gap-1 rounded-(--radius-control) border px-2 py-1.5",
                  i === sel ? "border-accent bg-accent-soft" : "border-border",
                )}
              >
                <button
                  type="button"
                  onClick={() => {
                    setSel(i);
                    cursor.current = null;
                  }}
                  className="flex-1 truncate text-left text-sm"
                  aria-current={i === sel}
                >
                  <span className="font-medium">{i === 0 ? "Email 1" : `Follow-up ${i}`}</span>
                  <span className="block truncate text-xs text-muted">
                    {(i === 0 || !thread ? s.subject : "") || body(s).slice(0, 40)}
                  </span>
                </button>
                {i > 0 && (
                  <>
                    <button
                      type="button"
                      aria-label={`Move step ${i + 1} up`}
                      className="rounded p-1 text-muted hover:bg-surface-2"
                      onClick={() => move(i, -1)}
                      disabled={i < 2}
                    >
                      <ArrowUp size={13} />
                    </button>
                    <button
                      type="button"
                      aria-label={`Move step ${i + 1} down`}
                      className="rounded p-1 text-muted hover:bg-surface-2"
                      onClick={() => move(i, 1)}
                      disabled={i === steps.length - 1}
                    >
                      <ArrowDown size={13} />
                    </button>
                  </>
                )}
                {steps.length > 1 && (
                  <button
                    type="button"
                    aria-label={`Delete step ${i + 1}`}
                    className="rounded p-1 text-muted hover:bg-surface-2"
                    onClick={() => setDel(i)}
                  >
                    <Trash2 size={13} />
                  </button>
                )}
              </div>
            </div>
          ))}
          <Button
            variant="secondary"
            className="mt-2"
            onClick={addStep}
            disabled={steps.length >= 10}
          >
            <Plus size={14} /> Add follow-up
          </Button>
          <div className="mt-4">
            <TextArea
              label="About your offer"
              maxLength={4000}
              value={brief}
              onChange={(e) => setBrief(e.target.value)}
              onBlur={() =>
                brief !== (seq.brief ?? "") &&
                http
                  .patch("/v1/followups/sequences/{id}", { path: { id: seq.id }, body: { brief } })
                  .catch(() => {})
              }
              help="The AI reads this when it writes AI parts."
            />
          </div>
        </aside>

        {/* editor */}
        <section className="flex min-w-0 flex-col gap-3" aria-label="Email editor">
          <div className="flex items-center justify-between">
            <h2 className="font-semibold">{sel === 0 ? "Email 1" : `Follow-up ${sel}`}</h2>
            <div className="flex items-center gap-3">
              <span className="text-xs text-muted" role="status" aria-live="polite">
                {save === "saving"
                  ? "Saving..."
                  : save === "dirty"
                    ? "Not saved yet"
                    : save === "error"
                      ? "Not saved"
                      : "Saved"}
              </span>
              <div
                role="group"
                aria-label="Body type"
                className="flex rounded-(--radius-control) border border-border p-0.5 text-sm"
              >
                {(["Text", "HTML"] as const).map((m) => (
                  <button
                    key={m}
                    type="button"
                    aria-pressed={cur.html === (m === "HTML")}
                    onClick={() => {
                      if (cur.html === (m === "HTML")) return;
                      if (m === "HTML") patch({ html: true, body_html: textToHtml(cur.body_text) });
                      else setToText(true);
                    }}
                    className={cn(
                      "rounded px-2.5 py-0.5",
                      cur.html === (m === "HTML") && "bg-accent-soft font-medium",
                    )}
                  >
                    {m}
                  </button>
                ))}
              </div>
            </div>
          </div>
          {save === "error" && saveError && <Notice tone="danger">{saveError}</Notice>}
          {showSubject ? (
            <Field
              label="Subject"
              value={cur.subject}
              onChange={(e) => patch({ subject: e.target.value })}
              placeholder="Quick question, {{first_name|there}}"
            />
          ) : (
            <p className="text-sm text-muted">
              Sent as a reply in the same thread (Re: + the first subject).
            </p>
          )}
          {cur.html && (
            <div className="flex gap-1" role="toolbar" aria-label="Format">
              <Button
                variant="secondary"
                className="h-7 font-bold"
                onClick={() => wrap("<b>", "</b>")}
                aria-label="Bold"
              >
                B
              </Button>
              <Button
                variant="secondary"
                className="h-7 italic"
                onClick={() => wrap("<i>", "</i>")}
                aria-label="Italic"
              >
                I
              </Button>
              <Button
                variant="secondary"
                className="h-7"
                onClick={() => {
                  const u = window.prompt("Link address (https://...)");
                  if (u) wrap(`<a href="${u.replace(/"/g, "")}">`, "</a>");
                }}
              >
                Link
              </Button>
              <Button
                variant="secondary"
                className="h-7"
                onClick={() => wrap("<ul><li>", "</li></ul>")}
              >
                List
              </Button>
              <Button
                variant="secondary"
                className="h-7"
                onClick={() => wrap("<ol><li>", "</li></ol>")}
              >
                Numbered
              </Button>
            </div>
          )}
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">Body</span>
            <textarea
              ref={area}
              value={body(cur)}
              rows={14}
              spellCheck
              onChange={(e) => setBody(e.target.value)}
              onSelect={(e) => {
                cursor.current = (e.target as HTMLTextAreaElement).selectionStart;
              }}
              onBlur={(e) => {
                cursor.current = e.target.selectionStart;
              }}
              className={cn(
                "w-full rounded-(--radius-control) border border-border bg-surface-2 p-3 leading-relaxed",
                cur.html && "font-mono text-[13px]",
              )}
            />
          </label>
          {cur.html && (
            <iframe
              title="How the email looks"
              sandbox=""
              srcDoc={sanitizeHtml(cur.body_html ?? "")}
              className="h-40 w-full rounded border border-border bg-white"
            />
          )}
          {parseError && <Notice tone="danger">{parseError}</Notice>}

          <div className="flex flex-wrap items-center gap-2">
            <MenuRoot>
              <MenuTrigger className="inline-flex h-9 items-center gap-2 rounded-(--radius-control) border border-border bg-surface px-3 text-sm font-medium hover:bg-surface-2">
                <Braces size={14} /> Variable
              </MenuTrigger>
              <MenuContent align="start" className="max-h-80 overflow-auto">
                <MenuLabel>Lead</MenuLabel>
                {LEAD_VARS.map((v) => (
                  <MenuItem key={v} onSelect={() => insert(`{{${v}}}`)}>
                    {v}
                  </MenuItem>
                ))}
                {(fields.data?.length ?? 0) > 0 && (
                  <>
                    <MenuSeparator />
                    <MenuLabel>Custom fields</MenuLabel>
                    {fields.data?.map((f) => (
                      <MenuItem key={f.key} onSelect={() => insert(`{{${f.key}}}`)}>
                        {f.label || f.key}
                      </MenuItem>
                    ))}
                  </>
                )}
                <MenuSeparator />
                <MenuLabel>Sender</MenuLabel>
                <MenuItem onSelect={() => insert("{{sender_name}}")}>sender_name</MenuItem>
                <MenuItem onSelect={() => insert("{{sender_email}}")}>sender_email</MenuItem>
                <MenuSeparator />
                <MenuLabel>Recipient</MenuLabel>
                <MenuItem onSelect={() => insert("{{email}}")}>email</MenuItem>
              </MenuContent>
            </MenuRoot>
            <Button
              variant="secondary"
              className="text-ai"
              disabled={!aiReady}
              title={aiReady ? undefined : "Set up AI first"}
              onClick={() => setAiDialog({ slot: null })}
            >
              <Sparkles size={14} /> {aiReady ? "AI part" : "Set up AI first"}
            </Button>
            <Button variant="secondary" onClick={() => setPreview(true)}>
              Preview
            </Button>
          </div>

          {(vars.length > 0 || keys.length > 0) && (
            <div className="flex flex-wrap gap-1.5" aria-label="Variables in this email">
              {vars.map((v, i) => (
                <VarChip
                  key={`${v.raw}-${i}`}
                  name={v.name}
                  fallback={
                    v.raw.includes("|") ? v.raw.slice(v.raw.indexOf("|") + 1, -2).trim() : ""
                  }
                  unknown={!known.has(v.name)}
                  onFallback={(fb) =>
                    setSteps((all) =>
                      all.map((s, k) =>
                        k === sel
                          ? {
                              ...s,
                              subject: withFallback(s.subject, v.raw, fb),
                              body_text: withFallback(s.body_text, v.raw, fb),
                              body_html: s.body_html ? withFallback(s.body_html, v.raw, fb) : null,
                            }
                          : s,
                      ),
                    )
                  }
                />
              ))}
              {keys.map((k) => (
                <button
                  key={k}
                  type="button"
                  onClick={() =>
                    setAiDialog({
                      slot: cur.ai_slots.find((s) => s.key === k) ?? { key: k, instruction: "" },
                    })
                  }
                  className="inline-flex items-center gap-1 rounded-full bg-ai/15 px-2.5 py-0.5 text-xs font-medium text-ai"
                >
                  <Sparkles size={11} /> AI: {k}
                </button>
              ))}
            </div>
          )}
        </section>
      </div>

      <AiPartDialog
        open={!!aiDialog}
        onClose={() => setAiDialog(null)}
        initial={aiDialog?.slot?.instruction ? aiDialog.slot : null}
        takenKeys={cur.ai_slots.map((s) => s.key)}
        onSave={(slot, isNew) => {
          setSteps((all) =>
            all.map((s, i) =>
              i === sel
                ? {
                    ...s,
                    ai_slots: isNew
                      ? [...s.ai_slots, slot]
                      : s.ai_slots.map((x) => (x.key === slot.key ? slot : x)),
                  }
                : s,
            ),
          );
          if (isNew) insert(`{{ai:${slot.key}}}`);
        }}
      />
      <ConfirmDialog
        open={del !== null}
        onClose={() => setDel(null)}
        title="Delete this step?"
        text="The step and its text are removed."
        confirmLabel="Delete step"
        onConfirm={() => {
          if (del === null) return;
          setSteps((all) => all.filter((_, i) => i !== del));
          setSel(0);
        }}
      />
      <ConfirmDialog
        open={toText}
        onClose={() => setToText(false)}
        title="Switch to plain text?"
        text="Formatting will be removed."
        confirmLabel="Switch to text"
        danger={false}
        onConfirm={() =>
          patch({
            html: false,
            body_text: htmlToText(sanitizeHtml(cur.body_html ?? "")),
            body_html: null,
          })
        }
      />
      <PreviewDrawer
        open={preview}
        onClose={() => setPreview(false)}
        sequenceId={seq.id}
        campaignId={campaign.id}
        accounts={campaign.accounts}
        userEmail={userEmail}
        texts={steps.map((s) => `${s.subject}\n${body(s)}`)}
      />
      <WriteWithAi
        open={writeAi}
        onClose={() => setWriteAi(false)}
        onUse={(list) => {
          setSteps(list.map(fromInput));
          setSel(0);
        }}
      />
    </div>
  );
}

function VarChip({
  name,
  fallback,
  unknown,
  onFallback,
}: {
  name: string;
  fallback: string;
  unknown: boolean;
  onFallback: (fb: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [v, setV] = useState(fallback);
  useEffect(() => setV(fallback), [fallback]);
  return (
    <span className="relative">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        title={
          unknown
            ? "We do not know this name. Check the spelling, or give a value when you enroll."
            : "Set a fallback"
        }
        className={cn(
          "rounded-full border px-2.5 py-0.5 font-mono text-xs",
          unknown
            ? "border-danger text-danger underline decoration-wavy"
            : "border-border bg-surface-2",
        )}
      >
        {name}
        {fallback ? ` | ${fallback}` : ""}
      </button>
      {open && (
        <span className="absolute left-0 top-full z-20 mt-1 flex w-56 flex-col gap-1.5 rounded-(--radius-card) border border-border bg-surface p-2 shadow-sm">
          <label className="text-xs">
            If empty, use:
            <input
              value={v}
              onChange={(e) => setV(e.target.value)}
              className="mt-1 h-8 w-full rounded border border-border bg-surface-2 px-2 text-sm"
            />
          </label>
          <span className="flex gap-1.5">
            <Button
              className="h-7"
              onClick={() => {
                onFallback(v);
                setOpen(false);
              }}
            >
              Set
            </Button>
            <Button
              variant="ghost"
              className="h-7"
              onClick={() => {
                onFallback("");
                setOpen(false);
              }}
            >
              Clear
            </Button>
          </span>
        </span>
      )}
    </span>
  );
}

function WriteWithAi({
  open,
  onClose,
  onUse,
}: {
  open: boolean;
  onClose: () => void;
  onUse: (s: S["StepInput"][]) => void;
}) {
  const [brief, setBrief] = useState<Brief>(EMPTY_BRIEF);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<S["StepInput"][] | null>(null);
  const [confirm, setConfirm] = useState(false);
  async function write() {
    setBusy(true);
    setError(null);
    try {
      const { steps, delay_days, ...b } = brief;
      const r = await http.post<S["DraftReply"]>("/v1/generator/sequences/draft", {
        body: { brief: b, steps, delay_days, save: false },
      });
      setDraft(r.steps);
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="xl"
      title="Write the emails with AI"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Close
          </Button>
          {draft && <Button onClick={() => setConfirm(true)}>Use these steps</Button>}
        </>
      }
    >
      <div className="grid gap-5 lg:grid-cols-2">
        <div className="flex flex-col gap-3">
          <BriefForm value={brief} onChange={setBrief} />
          <div>
            <Button busy={busy} disabled={!brief.product.trim()} onClick={write}>
              {busy ? "Writing..." : "Write emails"}
            </Button>
          </div>
          <Notice>This uses AI tokens.</Notice>
          {error && <Notice tone="danger">{error}</Notice>}
        </div>
        <div className="flex flex-col gap-3">
          {draft?.map((s, i) => (
            <article
              key={`${i}-${s.subject}`}
              className="rounded-(--radius-card) border border-border p-3 text-sm"
            >
              <Pill>{i === 0 ? "Email 1" : `Follow-up ${i}`}</Pill>
              {s.subject && <p className="mt-1 font-medium">{s.subject}</p>}
              <pre className="mt-1 whitespace-pre-wrap font-sans text-muted">{s.body_text}</pre>
            </article>
          ))}
          {!draft && <p className="text-muted">The emails show here when they are written.</p>}
        </div>
      </div>
      <ConfirmDialog
        open={confirm}
        onClose={() => setConfirm(false)}
        title="Replace all steps?"
        text="Your current steps are replaced by these."
        danger={false}
        confirmLabel="Replace steps"
        onConfirm={() => {
          if (draft) onUse(draft);
          onClose();
        }}
      />
    </Modal>
  );
}
