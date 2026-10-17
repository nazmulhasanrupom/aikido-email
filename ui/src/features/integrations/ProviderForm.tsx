import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { ApiProblem, messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { type GenProvider, q } from "@/api/queries";
import { Button, Field, Notice, Select } from "@/components/ui";

export const KINDS = [
  { id: "anthropic", label: "Anthropic", kind: "anthropic", base: "", model: "claude-haiku-5-5" },
  {
    id: "openai",
    label: "OpenAI",
    kind: "openai_compatible",
    base: "https://api.openai.com/v1",
    model: "",
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    kind: "openai_compatible",
    base: "https://openrouter.ai/api/v1",
    model: "",
  },
  {
    id: "ollama",
    label: "Ollama (on this computer)",
    kind: "openai_compatible",
    base: "http://localhost:11434/v1",
    model: "",
  },
  { id: "other", label: "Other OpenAI-compatible", kind: "openai_compatible", base: "", model: "" },
] as const;

export const PRIVACY =
  "Lead data goes to the provider you choose: name, job title, company, website, location, custom fields, notes and the last two emails sent. Email addresses, phone numbers and reply texts are not sent. Use Ollama to keep it on your own computer.";

/** Add or edit a model provider. The key is write only: it is never shown again. */
export function ProviderForm({
  existing,
  onSaved,
  onCancel,
}: {
  existing?: GenProvider | undefined;
  onSaved: (p: GenProvider, test: { ok: boolean; detail: string }) => void;
  onCancel?: () => void;
}) {
  const qc = useQueryClient();
  const start =
    KINDS.find((k) =>
      existing
        ? k.kind === existing.kind &&
          (k.base === existing.base_url || k.id === "other" || k.id === "anthropic")
        : false,
    ) ?? KINDS[0];
  const [kindId, setKindId] = useState<string>(start.id);
  const kind = KINDS.find((k) => k.id === kindId) ?? KINDS[0];
  const [name, setName] = useState(existing?.name ?? "");
  const [model, setModel] = useState(existing?.model ?? kind.model);
  const [base, setBase] = useState(existing?.base_url || kind.base);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function pick(id: string) {
    const k = KINDS.find((x) => x.id === id) ?? KINDS[0];
    setKindId(id);
    setBase(k.base);
    if (!existing) setModel(k.model);
  }

  async function save() {
    setBusy(true);
    setError(null);
    try {
      const body = {
        name: name || kind.label,
        model,
        ...(key ? { api_key: key } : {}),
        ...(kind.kind === "openai_compatible" ? { base_url: base } : {}),
      };
      const saved = existing
        ? await http.patch<GenProvider>("/v1/generator/providers/{id}", {
            path: { id: existing.id },
            body,
          })
        : await http.post<GenProvider>("/v1/generator/providers", {
            body: { kind: kind.kind, ...body },
          });
      let test = { ok: false, detail: "" };
      try {
        test = await http.post<{ ok: boolean; detail: string }>(
          "/v1/generator/providers/{id}/test",
          { path: { id: saved.id } },
        );
      } catch (e) {
        test = { ok: false, detail: messageOf(e) };
      }
      await qc.invalidateQueries({ queryKey: q.providers.queryKey });
      onSaved(saved, test);
    } catch (e) {
      setError(e instanceof ApiProblem ? e.message : messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      {!existing && (
        <Select
          label="Provider"
          value={kindId}
          options={KINDS.map((k) => ({ value: k.id, label: k.label }))}
          onChange={pick}
        />
      )}
      <Field
        label="Name"
        value={name}
        placeholder={kind.label}
        onChange={(e) => setName(e.target.value)}
      />
      <Field
        label="Model"
        value={model}
        onChange={(e) => setModel(e.target.value)}
        help="The model id, for example claude-haiku-5-5."
      />
      {kind.kind === "openai_compatible" && (
        <Field
          label="Base address"
          value={base}
          onChange={(e) => setBase(e.target.value)}
          help="We add /chat/completions."
        />
      )}
      <Field
        label={existing ? "New key (leave empty to keep the old one)" : "API key"}
        type="password"
        autoComplete="off"
        value={key}
        onChange={(e) => setKey(e.target.value)}
        help="The key is stored encrypted and never shown again."
      />
      <Notice>{PRIVACY}</Notice>
      {error && <Notice tone="danger">{error}</Notice>}
      <div className="flex gap-2">
        {onCancel && (
          <Button variant="secondary" onClick={onCancel}>
            Cancel
          </Button>
        )}
        <Button
          busy={busy}
          onClick={save}
          disabled={!model || (kind.kind === "openai_compatible" && !base)}
        >
          Save and test
        </Button>
      </div>
    </div>
  );
}
