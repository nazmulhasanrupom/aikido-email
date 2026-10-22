import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowDown,
  ArrowUp,
  Brush,
  Check,
  Code,
  Copy,
  Eye,
  Image,
  Link,
  Monitor,
  Plus,
  Power,
  Rocket,
  Smartphone,
  Sparkles,
  Stamp,
  Trash2,
  Type,
  UserRound,
  Zap,
} from "lucide-react";
import {
  type ButtonHTMLAttributes,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { toast } from "sonner";
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
import { AiPanel } from "./AiPanel";
import { AiPartDialog } from "./AiPartDialog";
import { EmailScore } from "./EmailScore";
import { ModeSwitch } from "./ModeSwitch";
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
  /** AI mode: the user prompt of this step. */
  ai_user_prompt: string;
  /** Variants only. */
  name?: string;
  active?: boolean;
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
  ai_user_prompt: s.ai_user_prompt ?? "",
});
const fromVariant = (v: S["StepVariant"]): Draft => ({
  id: uid(),
  subject: v.subject,
  body_text: v.body_text ?? "",
  body_html: v.body_html ?? null,
  html: !!v.body_html,
  delay_amount: 0,
  delay_type: "business_days",
  ai_slots: v.ai_slots,
  ai_user_prompt: "",
  name: v.name,
  active: v.active,
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
  ai_user_prompt: s.ai_user_prompt ?? "",
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
  const [variants, setVariants] = useState<Draft[]>(() => campaign.variants.map(fromVariant));
  // -1 is variant A (step 1 itself); 0 to 3 are B to E.
  const [vsel, setVsel] = useState(-1);
  const [saveDialog, setSaveDialog] = useState<null | { edited: number }>(null);
  const [saving, setSaving] = useState(false);
  const [footer, setFooter] = useState(campaign.footer ?? "");
  const [aiSystem, setAiSystem] = useState(campaign.sequence.ai_system_prompt ?? "");
  const savedSystem = useRef(campaign.sequence.ai_system_prompt ?? "");
  const aiMode = campaign.content_mode === "ai";
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
  const subjectRef = useRef<HTMLInputElement>(null);
  const [device, setDevice] = useState<"desktop" | "mobile">("desktop");
  const cursor = useRef<number | null>(null);
  const onVariant = sel === 0 && vsel >= 0 && !!variants[vsel];
  const cur = onVariant
    ? (variants[vsel] as Draft)
    : (steps[Math.min(sel, steps.length - 1)] ?? steps[0]);
  const slotProvider = gen.data?.settings.slot_provider_id;
  const aiReady =
    !!slotProvider && providers.data?.some((p) => p.id === slotProvider && p.status === "ok");

  const body = useCallback((s: Draft) => (s.html ? (s.body_html ?? "") : s.body_text), []);

  // What the server has (as text), what is being saved now, and the latest edits.
  const snapshot = JSON.stringify({ steps, variants, aiSystem });
  const lastSaved = useRef(snapshot);
  const inflight = useRef<string | null>(null);
  const snapshotRef = useRef(snapshot);
  snapshotRef.current = snapshot;

  const payloadOf = (d: Draft, i: number) => {
    const html = d.html ? sanitizeHtml(d.body_html ?? "") : null;
    return {
      subject: i > 0 && thread ? "" : d.subject,
      body_text: d.html ? htmlToText(html ?? "") : d.body_text,
      body_html: html,
      delay_amount: i === 0 ? 0 : d.delay_amount,
      delay_type: d.delay_type,
      ai_slots: d.ai_slots,
      ai_user_prompt: d.ai_user_prompt,
    };
  };
  /** Save the template (the steps and the variants of step 1). */
  const persist = useCallback(async () => {
    const snap = JSON.stringify({ steps, variants, aiSystem });
    inflight.current = snap;
    try {
      await persistNow();
      lastSaved.current = snap;
    } finally {
      inflight.current = null;
    }
  }, [steps, variants, thread, aiSystem]);
  const persistNow = async () => {
    if (aiSystem !== savedSystem.current) {
      await http.patch("/v1/followups/sequences/{id}", {
        path: { id: seq.id },
        body: { ai_system_prompt: aiSystem },
      });
      savedSystem.current = aiSystem;
    }
    await http.put(`/v1/followups/sequences/{id}/steps`, {
      path: { id: seq.id },
      body: {
        steps: steps.map(payloadOf),
        variants: variants.map((v) => {
          const { delay_amount: _d, delay_type: _t, ...rest } = payloadOf(v, 0);
          return { ...rest, name: v.name ?? "", active: v.active ?? true };
        }),
      },
    });
    void qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] });
  };

  const persistRef = useRef(persist);
  persistRef.current = persist;
  const unsaved = () =>
    snapshotRef.current !== lastSaved.current && snapshotRef.current !== inflight.current;

  // Autosave 1.5 s after the last change.
  useEffect(() => {
    if (snapshot === lastSaved.current) {
      setSave((s) => (s === "dirty" ? "saved" : s));
      return;
    }
    setSave("dirty");
    const t = setTimeout(async () => {
      setSave("saving");
      try {
        await persistRef.current();
        setSave(snapshotRef.current === lastSaved.current ? "saved" : "dirty");
        setSaveError(null);
      } catch (e) {
        setSave("error");
        setSaveError(
          e instanceof ApiProblem
            ? e.code === "missing_variable"
              ? e.message
              : (PROBLEM_WORDS[e.code] ?? e.message)
            : messageOf(e),
        );
      }
    }, 1500);
    return () => clearTimeout(t);
  }, [snapshot]);

  // Leaving the tab with a change that is not saved yet: save it now.
  useEffect(
    () => () => {
      if (unsaved()) void persistRef.current().catch(() => {});
    },
    [],
  );
  // Closing or reloading the page with unsaved changes: ask the browser to warn.
  useEffect(() => {
    const h = (e: BeforeUnloadEvent) => {
      if (snapshotRef.current === lastSaved.current && inflight.current === null) return;
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", h);
    return () => window.removeEventListener("beforeunload", h);
  }, []);

  /** Save: the template, then the emails of every lead. */
  async function saveEmails(overwrite?: "keep_edited" | "all") {
    setSaving(true);
    try {
      await persist();
      setSave("saved");
      if (aiMode) {
        toast.success("Prompts saved. Use Generate to write the emails.");
        return;
      }
      const body = { overwrite: overwrite ?? "keep_edited" };
      if (!overwrite) {
        const est = await http.post<S["Estimate"]>("/v1/campaigns/{id}/emails/estimate", {
          path: { id: campaign.id },
          body,
        });
        if (est.edited_by_hand > 0) {
          setSaveDialog({ edited: est.edited_by_hand });
          return;
        }
      }
      const run = await http.post<S["EmailRun"]>("/v1/campaigns/{id}/emails/render", {
        path: { id: campaign.id },
        body,
      });
      setSaveDialog(null);
      toast.success(
        run.done === 0 && run.failed === 0
          ? "Saved. No leads need emails yet."
          : `${run.done} email${run.done === 1 ? "" : "s"} saved${run.failed ? `, ${run.failed} could not be written (missing values)` : ""}.`,
      );
      void qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] });
    } catch (e) {
      toast.error(messageOf(e));
    } finally {
      setSaving(false);
    }
  }
  const saveRef = useRef(saveEmails);
  saveRef.current = saveEmails;
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void saveRef.current();
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, []);

  const patch = (p: Partial<Draft>) =>
    onVariant
      ? setVariants((all) => all.map((s, i) => (i === vsel ? { ...s, ...p } : s)))
      : setSteps((all) => all.map((s, i) => (i === sel ? { ...s, ...p } : s)));
  function addVariant(copy?: Draft) {
    if (variants.length >= 4) return;
    const base = copy ?? (steps[0] as Draft);
    const key = ["B", "C", "D", "E"][variants.length];
    setVariants([
      ...variants,
      {
        ...base,
        id: uid(),
        name: copy ? `${copy.name || "Copy"} copy` : (key ?? ""),
        active: true,
      },
    ]);
    setVsel(variants.length);
  }
  function removeVariant() {
    setVariants((all) => all.filter((_, i) => i !== vsel));
    setVsel(-1);
  }
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
  /** Put text into the subject where the cursor was (or at the end). */
  function insertSubject(text: string) {
    if (!cur) return;
    const el = subjectRef.current;
    const at = el?.selectionStart ?? cur.subject.length;
    patch({ subject: cur.subject.slice(0, at) + text + cur.subject.slice(at) });
    requestAnimationFrame(() => {
      el?.focus();
      el?.setSelectionRange(at + text.length, at + text.length);
    });
  }
  function addLink() {
    const u = window.prompt("Link address (https://...)")?.trim();
    if (!u) return;
    if (cur?.html) wrap(`<a href="${u.replace(/"/g, "")}">`, "</a>");
    else insert(u);
  }
  function addImage() {
    const u = window.prompt("Picture address (https://...)")?.trim();
    if (!u) return;
    if (!/^https:\/\//i.test(u)) {
      toast.error("Use a picture address that starts with https://");
      return;
    }
    insert(`<img src="${u.replace(/"/g, "")}" alt="">`);
  }
  function setHtml(on: boolean) {
    if (!cur || cur.html === on) return;
    if (on) patch({ html: true, body_html: textToHtml(cur.body_text) });
    else setToText(true);
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
        ai_user_prompt: "",
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

  const stepName = sel === 0 ? "Email 1" : `Follow-up ${sel}`;
  const variantKeys = ["A", ...variants.map((_, i) => ["B", "C", "D", "E"][i] as string)];

  return (
    <div className="flex flex-col gap-4">
      {live && (
        <Notice tone="info">
          This campaign is live. Changes apply to emails not written yet. Sent emails do not change.
        </Notice>
      )}
      <div className="flex items-center gap-3">
        <ModeSwitch campaign={campaign} />
        <div className="ml-auto flex items-center gap-3">
          <span
            className="flex items-center gap-1.5 text-xs text-muted"
            role="status"
            aria-live="polite"
          >
            <span
              aria-hidden="true"
              className={cn(
                "size-1.5 rounded-full transition-colors duration-300",
                save === "saved" && "bg-success",
                save === "error" && "bg-danger",
                (save === "dirty" || save === "saving") && "animate-pulse bg-warning",
              )}
            />
            {save === "saving"
              ? "Saving..."
              : save === "dirty"
                ? "Not saved yet"
                : save === "error"
                  ? "Not saved"
                  : "Saved"}
          </span>
          <Button onClick={() => void saveEmails()} busy={saving} title="Ctrl/Cmd + S">
            Save
          </Button>
        </div>
      </div>
      {save === "error" && saveError && <Notice tone="danger">{saveError}</Notice>}
      <div className="grid gap-6 lg:grid-cols-[232px_1fr]">
        {/* steps rail */}
        <aside className="flex flex-col gap-1" aria-label="Steps">
          {steps.map((s, i) => (
            <div key={s.id} className="animate-rise-in" style={{ animationDelay: `${i * 40}ms` }}>
              {i > 0 && (
                <div className="my-1.5 flex items-center gap-1.5 border-l border-dashed border-border py-1 pl-3 text-xs text-muted">
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
                    className="h-7 w-12 rounded border border-border bg-(--input-bg) px-1.5 text-text"
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
                    className="h-7 rounded border border-border bg-(--input-bg) px-1 text-text"
                  >
                    <option value="business_days">business days</option>
                    <option value="calendar_days">calendar days</option>
                  </select>
                </div>
              )}
              <div
                className={cn(
                  "group flex items-center gap-1 rounded-(--radius-card) border px-2.5 py-2 transition-[background-color,border-color,box-shadow] duration-200",
                  i === sel
                    ? "border-accent/70 bg-accent-soft shadow-(--shadow-card)"
                    : "border-border bg-surface hover:border-text/20",
                )}
              >
                <button
                  type="button"
                  onClick={() => {
                    setSel(i);
                    cursor.current = null;
                  }}
                  className="min-w-0 flex-1 truncate text-left text-sm"
                  aria-current={i === sel}
                >
                  <span className="font-medium">{i === 0 ? "Email 1" : `Follow-up ${i}`}</span>
                  <span className="block truncate text-xs text-muted">
                    {(i === 0 || !thread ? s.subject : "") || body(s).slice(0, 40) || "Empty"}
                  </span>
                </button>
                <span className="flex opacity-0 transition-opacity duration-150 group-focus-within:opacity-100 group-hover:opacity-100">
                  {i > 0 && (
                    <>
                      <button
                        type="button"
                        aria-label={`Move step ${i + 1} up`}
                        className="rounded p-1 text-muted hover:bg-surface-2 hover:text-text disabled:opacity-30"
                        onClick={() => move(i, -1)}
                        disabled={i < 2}
                      >
                        <ArrowUp size={13} />
                      </button>
                      <button
                        type="button"
                        aria-label={`Move step ${i + 1} down`}
                        className="rounded p-1 text-muted hover:bg-surface-2 hover:text-text disabled:opacity-30"
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
                      className="rounded p-1 text-muted hover:bg-danger/15 hover:text-danger"
                      onClick={() => setDel(i)}
                    >
                      <Trash2 size={13} />
                    </button>
                  )}
                </span>
              </div>
            </div>
          ))}
          <Button
            variant="ghost"
            className="mt-1 justify-start border border-dashed border-border text-muted hover:text-text"
            onClick={addStep}
            disabled={steps.length >= 10}
          >
            <Plus size={14} /> Add follow-up
          </Button>
          {aiMode && (
            <Button
              variant="ghost"
              className="justify-start"
              disabled={!aiReady}
              title={aiReady ? undefined : "Set up an AI model first"}
              onClick={() => setWriteAi(true)}
            >
              <Sparkles size={14} /> Write with AI
            </Button>
          )}
          <div className="mt-5 flex flex-col gap-2 border-t border-border pt-4">
            <Fold title="About your offer" hint="The AI reads this when it writes AI parts.">
              <TextArea
                label="About your offer"
                maxLength={4000}
                value={brief}
                onChange={(e) => setBrief(e.target.value)}
                onBlur={() =>
                  brief !== (seq.brief ?? "") &&
                  http
                    .patch("/v1/followups/sequences/{id}", {
                      path: { id: seq.id },
                      body: { brief },
                    })
                    .catch(() => {})
                }
              />
            </Fold>
            <Fold
              title="Footer"
              hint="Added under every email: a signature or an opt-out line. Save writes it into the emails."
            >
              <TextArea
                label="Footer"
                maxLength={2000}
                rows={3}
                value={footer}
                placeholder={"{{sender_name}}"}
                onChange={(e) => setFooter(e.target.value)}
                onBlur={() =>
                  footer !== (campaign.footer ?? "") &&
                  http
                    .patch("/v1/campaigns/{id}", { path: { id: campaign.id }, body: { footer } })
                    .then(() => qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] }))
                    .catch((e) => toast.error(messageOf(e)))
                }
              />
            </Fold>
          </div>
        </aside>

        {/* editor */}
        {aiMode ? (
          <AiPanel
            campaign={campaign}
            stepIndex={Math.min(sel, steps.length - 1)}
            stepCount={steps.length}
            userPrompt={steps[Math.min(sel, steps.length - 1)]?.ai_user_prompt ?? ""}
            system={aiSystem}
            onSystem={setAiSystem}
            onUser={(v) =>
              setSteps((all) => all.map((x, i) => (i === sel ? { ...x, ai_user_prompt: v } : x)))
            }
            aiReady={!!aiReady}
            onPreset={(p) => {
              setAiSystem(p.system);
              setSteps((all) =>
                all.map((x, i) => ({
                  ...x,
                  ai_user_prompt: p.steps[Math.min(i, p.steps.length - 1)] ?? "",
                })),
              );
            }}
          />
        ) : (
          <div className="grid min-w-0 gap-6 xl:grid-cols-[minmax(0,1fr)_216px]">
            <section
              key={`${sel}:${vsel}`}
              className="animate-rise-in flex min-w-0 flex-col gap-3"
              aria-label={`Editor: ${stepName}`}
            >
              {/* subject */}
              {showSubject ? (
                <div className="flex h-12 items-center gap-2 rounded-(--radius-card) border border-border bg-surface pl-2 pr-1.5 shadow-(--shadow-card) focus-within:border-accent/70">
                  <label
                    htmlFor="email-subject"
                    className="flex shrink-0 items-center gap-1 rounded-md bg-accent-soft px-2 py-1 text-xs font-medium"
                  >
                    <Zap size={12} className="text-accent" aria-hidden="true" /> Subject
                  </label>
                  <input
                    id="email-subject"
                    ref={subjectRef}
                    value={cur.subject}
                    onChange={(e) => patch({ subject: e.target.value })}
                    placeholder="Quick question, {{first_name|there}}"
                    className="h-full min-w-0 flex-1 bg-transparent text-sm outline-none focus-visible:shadow-none"
                  />
                  <VarMenu fields={fields.data} onPick={insertSubject}>
                    <Tool
                      label="Add a variable to the subject"
                      className="size-8 border-0 shadow-none"
                    >
                      <Zap size={15} />
                    </Tool>
                  </VarMenu>
                </div>
              ) : (
                <div className="flex h-12 items-center rounded-(--radius-card) border border-dashed border-border px-4 text-sm text-muted">
                  Sent as a reply in the same thread (Re: + the first subject).
                </div>
              )}

              {/* body */}
              <div className="overflow-hidden rounded-(--radius-card) border border-border bg-surface shadow-(--shadow-card) transition-[border-color] duration-200 focus-within:border-accent/70">
                <div
                  className={cn(
                    "mx-auto transition-[max-width] duration-300 ease-(--ease-out-soft)",
                    device === "mobile" ? "max-w-[390px]" : "max-w-full",
                  )}
                >
                  <textarea
                    ref={area}
                    aria-label="Body"
                    value={body(cur)}
                    rows={16}
                    spellCheck
                    placeholder="Write your email..."
                    onChange={(e) => setBody(e.target.value)}
                    onSelect={(e) => {
                      cursor.current = (e.target as HTMLTextAreaElement).selectionStart;
                    }}
                    onBlur={(e) => {
                      cursor.current = e.target.selectionStart;
                    }}
                    className={cn(
                      "block min-h-[380px] w-full resize-y border-0 bg-transparent px-6 py-5 leading-relaxed outline-none focus-visible:shadow-none",
                      cur.html && "font-mono text-[13px]",
                      device === "mobile" && "border-x border-dashed border-border",
                    )}
                  />
                </div>
                {cur.html && (
                  <div className="border-t border-border bg-surface-2 p-3">
                    <p className="mb-2 text-xs font-medium text-muted">How the email looks</p>
                    <iframe
                      title="How the email looks"
                      sandbox=""
                      srcDoc={sanitizeHtml(cur.body_html ?? "")}
                      className={cn(
                        "mx-auto h-48 w-full rounded border border-border bg-white transition-[max-width] duration-300",
                        device === "mobile" ? "max-w-[390px]" : "max-w-full",
                      )}
                    />
                  </div>
                )}
              </div>

              {parseError && <Notice tone="danger">{parseError}</Notice>}
              <EmailScore
                text={cur.html ? htmlToText(sanitizeHtml(cur.body_html ?? "")) : cur.body_text}
              />

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
                                  body_html: s.body_html
                                    ? withFallback(s.body_html, v.raw, fb)
                                    : null,
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
                          slot: cur.ai_slots.find((s) => s.key === k) ?? {
                            key: k,
                            instruction: "",
                          },
                        })
                      }
                      className="inline-flex items-center gap-1 rounded-full bg-ai/15 px-2.5 py-0.5 text-xs font-medium text-ai"
                    >
                      <Sparkles size={11} /> AI: {k}
                    </button>
                  ))}
                </div>
              )}

              <div className="flex justify-center pt-2">
                <Tool
                  label="Add follow-up"
                  onClick={addStep}
                  disabled={steps.length >= 10}
                  className="rounded-full hover:rotate-90"
                >
                  <Plus size={16} />
                </Tool>
              </div>
            </section>

            {/* variants of the first email */}
            {sel === 0 ? (
              <aside aria-label="Variants of the first email" className="flex flex-col gap-2">
                <button
                  type="button"
                  onClick={() => addVariant()}
                  disabled={variants.length >= 4}
                  className="flex h-12 items-center gap-2 rounded-(--radius-card) border border-border bg-surface px-4 text-sm font-medium shadow-(--shadow-card) hover:border-text/20 hover:bg-surface-2 disabled:opacity-50"
                >
                  <Plus size={15} aria-hidden="true" /> Add variant
                </button>
                {variantKeys.map((k, idx) => {
                  const v = idx === 0 ? null : variants[idx - 1];
                  const on = vsel === idx - 1;
                  return (
                    <div key={k} className="animate-rise-in relative">
                      {on && (
                        <span
                          aria-hidden="true"
                          className="absolute top-6 -left-6 hidden w-6 border-t-2 border-dotted border-accent xl:block"
                        />
                      )}
                      <div
                        className={cn(
                          "rounded-(--radius-card) border bg-surface transition-[border-color,box-shadow,background-color] duration-200",
                          on
                            ? "border-accent shadow-(--shadow-pop)"
                            : "border-border shadow-(--shadow-card) hover:border-text/20",
                          v && v.active === false && "opacity-60",
                        )}
                      >
                        <button
                          type="button"
                          aria-pressed={on}
                          onClick={() => setVsel(idx - 1)}
                          className="flex h-12 w-full items-center gap-2 px-4 text-left text-sm font-medium"
                        >
                          <span className="min-w-0 flex-1 truncate">
                            Edit variant {k}
                            {v?.name && v.name !== k ? (
                              <span className="font-normal text-muted"> · {v.name}</span>
                            ) : null}
                          </span>
                          {v && v.active === false && <Pill>Off</Pill>}
                          {on && (
                            <Check size={16} className="shrink-0 text-accent" aria-hidden="true" />
                          )}
                        </button>
                        {on && v && (
                          <div className="animate-fade-in flex flex-col gap-2 border-t border-border px-3 py-3">
                            <Field
                              label="Variant name"
                              value={v.name ?? ""}
                              maxLength={40}
                              onChange={(e) => patch({ name: e.target.value })}
                            />
                            <div className="flex gap-1">
                              <Tool
                                label="Duplicate"
                                className="size-8"
                                onClick={() => addVariant(v)}
                                disabled={variants.length >= 4}
                              >
                                <Copy size={14} />
                              </Tool>
                              <Tool
                                label={v.active === false ? "Turn on" : "Turn off"}
                                className="size-8"
                                onClick={() => patch({ active: !(v.active ?? true) })}
                              >
                                <Power size={14} />
                              </Tool>
                              <Tool
                                label="Delete"
                                className="size-8 hover:bg-danger/15 hover:text-danger"
                                onClick={removeVariant}
                              >
                                <Trash2 size={14} />
                              </Tool>
                            </div>
                            <p className="text-xs text-muted">
                              {v.active === false
                                ? "Off: gets no new leads."
                                : "Leads are split evenly between the variants."}
                            </p>
                          </div>
                        )}
                      </div>
                    </div>
                  );
                })}
              </aside>
            ) : (
              <aside className="hidden xl:block">
                <p className="rounded-(--radius-card) border border-dashed border-border p-4 text-xs text-muted">
                  A/B variants are for Email 1 only.
                </p>
              </aside>
            )}
          </div>
        )}
      </div>

      {/* tool bar */}
      {!aiMode && (
        <div
          role="toolbar"
          aria-label="Email tools"
          className="sticky bottom-0 z-20 -mx-6 -mb-6 mt-4 flex flex-wrap items-center justify-center gap-2 border-t border-border bg-bg/85 px-6 py-3 backdrop-blur-md"
        >
          <MenuRoot>
            <MenuTrigger asChild disabled={!cur.html}>
              <Tool label={cur.html ? "Format text" : "Switch to HTML to format text"}>
                <Type size={16} />
              </Tool>
            </MenuTrigger>
            <MenuContent side="top" align="start">
              <MenuItem onSelect={() => wrap("<b>", "</b>")}>
                <b>Bold</b>
              </MenuItem>
              <MenuItem onSelect={() => wrap("<i>", "</i>")}>
                <i>Italic</i>
              </MenuItem>
              <MenuItem onSelect={() => wrap("<ul><li>", "</li></ul>")}>List</MenuItem>
              <MenuItem onSelect={() => wrap("<ol><li>", "</li></ol>")}>Numbered list</MenuItem>
            </MenuContent>
          </MenuRoot>
          <VarMenu fields={fields.data} onPick={insert} side="top">
            <Tool label="Add a variable">
              <Zap size={16} />
            </Tool>
          </VarMenu>
          <Tool label="Add a link" onClick={addLink}>
            <Link size={16} />
          </Tool>
          <Tool
            label={cur.html ? "Add a picture" : "Switch to HTML to add a picture"}
            onClick={addImage}
            disabled={!cur.html}
          >
            <Image size={16} />
          </Tool>
          <Tool
            label={aiReady ? "Add an AI part" : "Set up AI first"}
            onClick={() => setAiDialog({ slot: null })}
            disabled={!aiReady}
            className="text-ai"
          >
            <Rocket size={16} />
          </Tool>
          <MenuRoot>
            <MenuTrigger asChild>
              <Tool label="Add sender details">
                <UserRound size={16} />
              </Tool>
            </MenuTrigger>
            <MenuContent side="top" align="start">
              <MenuItem onSelect={() => insert("{{sender_name}}")}>Sender name</MenuItem>
              <MenuItem onSelect={() => insert("{{sender_email}}")}>Sender email</MenuItem>
            </MenuContent>
          </MenuRoot>
          <Tool label="Add an unsubscribe link" onClick={() => insert("{{unsubscribe}}")}>
            <Stamp size={16} />
          </Tool>
          <span aria-hidden="true" className="mx-1 h-7 w-px bg-border" />
          <Tool
            label={aiReady ? "Write all emails with AI" : "Set up an AI model first"}
            onClick={() => setWriteAi(true)}
            disabled={!aiReady}
          >
            <Sparkles size={16} />
          </Tool>
          <Segment label="Width">
            <SegButton
              label="Desktop width"
              on={device === "desktop"}
              onClick={() => setDevice("desktop")}
            >
              <Monitor size={16} />
            </SegButton>
            <SegButton
              label="Phone width"
              on={device === "mobile"}
              onClick={() => setDevice("mobile")}
            >
              <Smartphone size={16} />
            </SegButton>
          </Segment>
          <Segment label="Body type">
            <SegButton label="Plain text" on={!cur.html} onClick={() => setHtml(false)}>
              <Brush size={16} />
            </SegButton>
            <SegButton label="HTML" on={cur.html} onClick={() => setHtml(true)}>
              <Code size={16} />
            </SegButton>
          </Segment>
          <span aria-hidden="true" className="mx-1 h-7 w-px bg-border" />
          <Tool label="Preview" onClick={() => setPreview(true)}>
            <Eye size={16} />
          </Tool>
        </div>
      )}

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
      <Modal
        open={!!saveDialog}
        onClose={() => setSaveDialog(null)}
        size="sm"
        title="Save emails"
        footer={
          <>
            <Button
              variant="secondary"
              onClick={() => void saveEmails("keep_edited")}
              busy={saving}
            >
              Keep my edits
            </Button>
            <Button variant="danger" onClick={() => void saveEmails("all")} busy={saving}>
              Replace them
            </Button>
          </>
        }
      >
        <p>
          Replace {saveDialog?.edited} email{saveDialog?.edited === 1 ? "" : "s"} you edited by
          hand? Sent emails never change.
        </p>
      </Modal>
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

/** A square icon button for the tool bar. The label is the tooltip and the name for screen readers. */
function Tool({
  label,
  className,
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={cn(
        "flex size-10 items-center justify-center rounded-(--radius-control) border border-border bg-surface text-text shadow-(--shadow-card) duration-200 hover:-translate-y-0.5 hover:bg-surface-2 hover:shadow-(--shadow-pop) active:translate-y-0 active:scale-95 disabled:pointer-events-none disabled:opacity-40 data-[state=open]:bg-surface-2",
        className,
      )}
      {...rest}
    >
      {children}
    </button>
  );
}

function Segment({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div
      role="group"
      aria-label={label}
      className="flex gap-0.5 rounded-(--radius-card) border border-border bg-surface p-1 shadow-(--shadow-card)"
    >
      {children}
    </div>
  );
}

function SegButton({
  label,
  on,
  onClick,
  children,
}: {
  label: string;
  on: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-pressed={on}
      onClick={onClick}
      className={cn(
        "flex size-8 items-center justify-center rounded-(--radius-control) duration-200",
        on ? "bg-accent-soft text-text shadow-(--shadow-card)" : "text-muted hover:text-text",
      )}
    >
      {children}
    </button>
  );
}

/** The menu of variables; `children` is the button that opens it. */
function VarMenu({
  fields,
  onPick,
  side,
  children,
}: {
  fields: S["LeadField"][] | undefined;
  onPick: (text: string) => void;
  side?: "top" | "bottom";
  children: ReactNode;
}) {
  return (
    <MenuRoot>
      <MenuTrigger asChild>{children}</MenuTrigger>
      <MenuContent side={side} align="start" className="max-h-80 overflow-auto">
        <MenuLabel>Lead</MenuLabel>
        {LEAD_VARS.map((v) => (
          <MenuItem key={v} onSelect={() => onPick(`{{${v}}}`)}>
            {v}
          </MenuItem>
        ))}
        {(fields?.length ?? 0) > 0 && (
          <>
            <MenuSeparator />
            <MenuLabel>Custom fields</MenuLabel>
            {fields?.map((f) => (
              <MenuItem key={f.key} onSelect={() => onPick(`{{${f.key}}}`)}>
                {f.label || f.key}
              </MenuItem>
            ))}
          </>
        )}
        <MenuSeparator />
        <MenuLabel>Sender</MenuLabel>
        <MenuItem onSelect={() => onPick("{{sender_name}}")}>sender_name</MenuItem>
        <MenuItem onSelect={() => onPick("{{sender_email}}")}>sender_email</MenuItem>
        <MenuSeparator />
        <MenuLabel>Recipient</MenuLabel>
        <MenuItem onSelect={() => onPick("{{email}}")}>email</MenuItem>
        <MenuItem onSelect={() => onPick("{{unsubscribe}}")}>unsubscribe link</MenuItem>
      </MenuContent>
    </MenuRoot>
  );
}

/** A section that opens and closes, to keep the steps list short. */
function Fold({ title, hint, children }: { title: string; hint: string; children: ReactNode }) {
  return (
    <details className="group rounded-(--radius-control) [&_label]:sr-only">
      <summary className="flex cursor-pointer list-none items-center justify-between rounded-(--radius-control) px-1 py-1.5 text-sm font-medium hover:text-accent [&::-webkit-details-marker]:hidden">
        {title}
        <Plus
          size={14}
          aria-hidden="true"
          className="text-muted transition-transform duration-200 group-open:rotate-45"
        />
      </summary>
      <div className="animate-rise-in flex flex-col gap-1.5 pt-1 pb-2">
        {children}
        <p className="text-xs text-muted">{hint}</p>
      </div>
    </details>
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
