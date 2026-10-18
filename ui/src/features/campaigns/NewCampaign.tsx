import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { Button, ErrorNote, Field, Modal, Notice, NumField, Select, TextArea } from "@/components/ui";
import { STARTERS } from "@/lib/starters";

export const STARTER = {
  subject: "Quick question, {{first_name|there}}",
  body_text: "Hi {{first_name|there}},\n\n\n\nBest,\n{{sender_name}}",
};

/** The brief the AI reads when it writes a whole sequence. */
export interface Brief {
  product: string;
  offer: string;
  audience: string;
  tone: string;
  language: string;
  footer: string;
  steps: number;
  delay_days: number;
}

export function BriefForm({ value, onChange }: { value: Brief; onChange: (v: Brief) => void }) {
  const set = (p: Partial<typeof value>) => onChange({ ...value, ...p });
  return (
    <div className="flex flex-col gap-3">
      <Field
        label="Your product"
        value={value.product}
        onChange={(e) => set({ product: e.target.value })}
      />
      <TextArea
        label="Your offer"
        value={value.offer}
        onChange={(e) => set({ offer: e.target.value })}
      />
      <Field
        label="Who you write to"
        value={value.audience}
        onChange={(e) => set({ audience: e.target.value })}
      />
      <div className="grid grid-cols-2 gap-3">
        <Field
          label="Tone"
          placeholder="friendly, short"
          value={value.tone}
          onChange={(e) => set({ tone: e.target.value })}
        />
        <Field
          label="Language"
          placeholder="English"
          value={value.language}
          onChange={(e) => set({ language: e.target.value })}
        />
        <NumField
          label="Number of emails"
          value={value.steps}
          min={1}
          max={10}
          onChange={(steps) => set({ steps })}
        />
        <NumField
          label="Days between emails"
          value={value.delay_days}
          min={1}
          max={365}
          onChange={(delay_days) => set({ delay_days })}
        />
      </div>
      <Field
        label="Footer (optional)"
        value={value.footer}
        onChange={(e) => set({ footer: e.target.value })}
      />
    </div>
  );
}

export const EMPTY_BRIEF: Brief = {
  product: "",
  offer: "",
  audience: "",
  tone: "",
  language: "",
  footer: "",
  steps: 3,
  delay_days: 3,
};

export function NewCampaign({
  open,
  onClose,
  ws,
}: {
  open: boolean;
  onClose: () => void;
  ws: string;
}) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const status = useQuery({ ...q.genSettings, enabled: open });
  const providers = useQuery({ ...q.providers, enabled: open });
  const draftProvider = status.data?.settings.draft_provider_id;
  const canAi =
    !!draftProvider && providers.data?.some((p) => p.id === draftProvider && p.status === "ok");
  const [name, setName] = useState("");
  const [mode, setMode] = useState<"blank" | "ai" | "starter">("blank");
  const [starterId, setStarterId] = useState(STARTERS[0]?.id ?? "");
  const [brief, setBrief] = useState<Brief>(EMPTY_BRIEF);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  async function create() {
    setBusy(true);
    setError(null);
    try {
      let created: S["CampaignDetail"];
      if (mode === "ai") {
        const { steps, delay_days, ...b } = brief;
        const draft = await http.post<S["DraftReply"]>("/v1/generator/sequences/draft", {
          body: { brief: b, steps, delay_days, name, save: true },
        });
        created = await http.post<S["CampaignDetail"]>("/v1/campaigns", {
          body: { name, sequence_id: draft.sequence_id },
        });
      } else if (mode === "starter") {
        const st = STARTERS.find((x) => x.id === starterId) ?? STARTERS[0];
        if (!st) throw new Error("No starter");
        created = await http.post<S["CampaignDetail"]>("/v1/campaigns", {
          body: { name, sequence: { name, steps: st.steps } },
        });
        if (st.kind === "ai") {
          await http.patch("/v1/followups/sequences/{id}", {
            path: { id: created.sequence_id },
            body: { ai_system_prompt: st.system ?? "" },
          });
          await http.patch("/v1/campaigns/{id}", { path: { id: created.id }, body: { content_mode: "ai" } });
        }
      } else {
        created = await http.post<S["CampaignDetail"]>("/v1/campaigns", {
          body: { name, sequence: { name, steps: [STARTER] } },
        });
      }
      await qc.invalidateQueries({ queryKey: ["campaigns"] });
      onClose();
      setName("");
      await navigate({ to: "/w/$ws/campaigns/$id/editor", params: { ws, id: created.id } });
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="New campaign"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            onClick={create}
            busy={busy}
            disabled={!name.trim() || (mode === "ai" && !brief.product.trim())}
          >
            {busy && mode === "ai" ? "Writing..." : "Create campaign"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Field label="Name" value={name} onChange={(e) => setName(e.target.value)} />
        <fieldset className="grid gap-2 sm:grid-cols-3">
          <legend className="mb-1.5 text-sm font-medium">Start with</legend>
          <label
            className={`cursor-pointer rounded-(--radius-card) border p-3 ${mode === "blank" ? "border-accent bg-accent-soft" : "border-border"}`}
          >
            <input
              type="radio"
              name="start"
              className="sr-only"
              checked={mode === "blank"}
              onChange={() => setMode("blank")}
            />
            <span className="font-medium">Blank</span>
            <span className="block text-xs text-muted">One email you write yourself.</span>
          </label>
          <label
            className={`rounded-(--radius-card) border p-3 ${mode === "ai" ? "border-accent bg-accent-soft" : "border-border"} ${canAi ? "cursor-pointer" : "opacity-60"}`}
          >
            <input
              type="radio"
              name="start"
              className="sr-only"
              checked={mode === "ai"}
              disabled={!canAi}
              onChange={() => setMode("ai")}
            />
            <span className="font-medium">Write with AI</span>
            <span className="block text-xs text-muted">
              {canAi
                ? "Tell the AI about your offer. It writes the emails."
                : "Set up an AI model first."}
            </span>
          </label>
          <label
            className={`cursor-pointer rounded-(--radius-card) border p-3 ${mode === "starter" ? "border-accent bg-accent-soft" : "border-border"}`}
          >
            <input
              type="radio"
              name="start"
              className="sr-only"
              checked={mode === "starter"}
              onChange={() => setMode("starter")}
            />
            <span className="font-medium">Starter kit</span>
            <span className="block text-xs text-muted">Ready emails or prompts to edit.</span>
          </label>
        </fieldset>
        {mode === "starter" && (
          <Select
            label="Starter"
            value={starterId}
            onChange={setStarterId}
            options={STARTERS.map((x) => ({ value: x.id, label: x.name }))}
            help={STARTERS.find((x) => x.id === starterId)?.help}
          />
        )}
        {mode === "ai" && (
          <>
            <BriefForm value={brief} onChange={setBrief} />
            <Notice>This uses AI tokens.</Notice>
          </>
        )}
        {error != null && <ErrorNote error={error} />}
      </div>
    </Modal>
  );
}
