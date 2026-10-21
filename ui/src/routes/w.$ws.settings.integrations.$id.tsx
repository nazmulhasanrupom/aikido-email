import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { type GenProvider, q } from "@/api/queries";
import {
  Button,
  Card,
  Check2,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Head,
  Modal,
  Notice,
  NumField,
  Pill,
  Select,
  Skeleton,
  Table,
  TagInput,
  TD,
} from "@/components/ui";
import { PRIVACY, ProviderForm } from "@/features/integrations/ProviderForm";
import { fmtInt } from "@/lib/format";
import { PROBLEM_WORDS } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/settings/integrations/$id")({
  component: IntegrationPage,
});

function IntegrationPage() {
  const { id, ws } = Route.useParams();
  if (id !== "ai")
    return (
      <div className="flex flex-col gap-2">
        <p>This integration is not ready yet.</p>
        <Link to="/w/$ws/settings/integrations" params={{ ws }} className="text-accent underline">
          Back to integrations
        </Link>
      </div>
    );
  return <AiModels />;
}

function AiModels() {
  const qc = useQueryClient();
  const providers = useQuery(q.providers);
  const gen = useQuery(q.genSettings);
  const leads = useQuery(q.leadSettings);
  const [form, setForm] = useState<GenProvider | "new" | null>(null);
  const [del, setDel] = useState<GenProvider | null>(null);
  const test = useMutation({
    mutationFn: (p: GenProvider) =>
      http.post<{ ok: boolean; detail: string }>("/v1/generator/providers/{id}/test", {
        path: { id: p.id },
      }),
    onSuccess: (r) => {
      r.ok ? toast.success("The model answers.") : toast.error(r.detail || "The test failed.");
      void qc.invalidateQueries({ queryKey: q.providers.queryKey });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const list = providers.data ?? [];
  const opts = [{ value: "", label: "None" }, ...list.map((p) => ({ value: p.id, label: p.name }))];
  return (
    <div className="flex flex-col gap-6">
      <section className="flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">Models</h2>
          <Button onClick={() => setForm("new")}>Add model</Button>
        </div>
        {providers.isPending && <Skeleton rows={2} />}
        {providers.isError && <ErrorNote error={providers.error} />}
        {providers.data?.length === 0 && (
          <EmptyState text="No AI model yet. Add one to use AI parts, the sequence writer and smart CSV mapping." />
        )}
        {list.length > 0 && (
          <Table>
            <Head cols={["Name", "Kind", "Model", "Status", ""]} />
            <tbody>
              {list.map((p) => (
                <tr key={p.id} className="border-t border-border">
                  <td className={`${TD} font-medium`}>{p.name}</td>
                  <td className={TD}>
                    {p.kind === "anthropic" ? "Anthropic" : "OpenAI-compatible"}
                  </td>
                  <td className={TD}>{p.model}</td>
                  <td className={TD}>
                    <Pill tone={p.status === "ok" ? "success" : "danger"}>
                      {p.status === "ok" ? "Working" : "Key refused"}
                    </Pill>
                  </td>
                  <td className={TD}>
                    <Button
                      variant="ghost"
                      className="h-7"
                      busy={test.isPending && test.variables?.id === p.id}
                      onClick={() => test.mutate(p)}
                    >
                      Test
                    </Button>
                    <Button variant="ghost" className="h-7" onClick={() => setForm(p)}>
                      Edit
                    </Button>
                    <Button variant="ghost" className="h-7" onClick={() => setDel(p)}>
                      Delete
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        )}
      </section>

      {gen.data && leads.data && (
        <Card className="flex max-w-2xl flex-col gap-3">
          <h2 className="font-semibold">Which model does what</h2>
          <Pick
            label="Writes AI parts"
            value={gen.data.settings.slot_provider_id ?? ""}
            options={opts}
            onSave={(v) =>
              http
                .patch("/v1/generator/settings", { body: { slot_provider_id: v || null } })
                .then(() => qc.invalidateQueries({ queryKey: ["generator"] }))
            }
          />
          <Pick
            label="Writes whole sequences"
            value={gen.data.settings.draft_provider_id ?? ""}
            options={opts}
            onSave={(v) =>
              http
                .patch("/v1/generator/settings", { body: { draft_provider_id: v || null } })
                .then(() => qc.invalidateQueries({ queryKey: ["generator"] }))
            }
          />
          <Pick
            label="Maps CSV columns"
            value={leads.data.settings.ai_mapping_provider_id ?? ""}
            options={[
              { value: "", label: "Same as AI parts" },
              ...list.map((p) => ({ value: p.id, label: p.name })),
            ]}
            onSave={(v) =>
              http
                .patch("/v1/leads/settings", { body: { ai_mapping_provider_id: v || null } })
                .then(() => qc.invalidateQueries({ queryKey: ["leads", "settings"] }))
            }
          />
        </Card>
      )}
      {gen.data && <Rules s={gen.data.settings} />}
      {leads.data && <MappingRules s={leads.data.settings} />}
      <Usage />
      <Notice>{PRIVACY}</Notice>

      <Modal
        open={!!form}
        onClose={() => setForm(null)}
        title={form === "new" ? "Add a model" : "Edit model"}
      >
        {form && (
          <ProviderForm
            existing={form === "new" ? undefined : form}
            onCancel={() => setForm(null)}
            onSaved={(_, t) => {
              setForm(null);
              t.ok
                ? toast.success("Saved. The model answers.")
                : toast.error(`Saved, but the test failed. ${t.detail}`);
            }}
          />
        )}
      </Modal>
      <ConfirmDialog
        open={!!del}
        onClose={() => setDel(null)}
        title="Delete this model?"
        text="It stops working for everything that uses it."
        confirmLabel="Delete model"
        onConfirm={async () => {
          try {
            await http.delEmpty("/v1/generator/providers/{id}", {
              path: { id: (del as GenProvider).id },
            });
            await qc.invalidateQueries({ queryKey: q.providers.queryKey });
          } catch (e) {
            throw new Error(
              e instanceof ApiProblem ? (PROBLEM_WORDS[e.code] ?? e.message) : messageOf(e),
            );
          }
        }}
      />
    </div>
  );
}

function Pick({
  label,
  value,
  options,
  onSave,
}: {
  label: string;
  value: string;
  options: { value: string; label: string }[];
  onSave: (v: string) => Promise<unknown>;
}) {
  return (
    <Select
      label={label}
      value={value}
      options={options}
      onChange={async (v) => {
        try {
          await onSave(v);
          toast.success("Saved.");
        } catch (e) {
          toast.error(messageOf(e));
        }
      }}
    />
  );
}

function Rules({ s: initial }: { s: S["GeneratorSettings"] }) {
  const qc = useQueryClient();
  const [s, setS] = useState(initial);
  useEffect(() => setS(initial), [initial]);
  const set = (p: Partial<S["GeneratorSettings"]>) => setS({ ...s, ...p });
  const price = (model: string) => s.prices[model] ?? { input: 0, output: 0 };
  const usage = useQuery({
    queryKey: ["generator", "usage", "models"],
    queryFn: () => http.get<S["UsageReply"]>("/v1/generator/usage"),
  });
  const models = [
    ...new Set([...(usage.data?.by_model.map((m) => m.key) ?? []), ...Object.keys(s.prices)]),
  ];
  return (
    <Card className="flex max-w-2xl flex-col gap-3">
      <h2 className="font-semibold">Rules for AI parts</h2>
      <Check2
        label="Review every AI text first"
        checked={s.review_required}
        onChange={(v) => set({ review_required: v })}
      />
      <div className="grid grid-cols-2 gap-3">
        <NumField
          label="Write this many hours before sending"
          value={s.lookahead_hours}
          min={1}
          onChange={(n) => set({ lookahead_hours: n })}
        />
        <Select
          label="When the AI fails"
          value={s.on_failure}
          onChange={(v) => set({ on_failure: v as S["OnFailure"] })}
          options={[
            { value: "fallback", label: "Use the fallback text" },
            { value: "wait", label: "Wait" },
          ]}
        />
        {s.on_failure === "wait" && (
          <NumField
            label="Wait up to"
            value={s.max_wait_mins}
            min={1}
            suffix="minutes"
            onChange={(n) => set({ max_wait_mins: n })}
          />
        )}
        <NumField
          label="Longest answer"
          value={s.max_output_tokens}
          min={50}
          suffix="tokens"
          onChange={(n) => set({ max_output_tokens: n })}
        />
        <NumField
          label="Creativity (temperature)"
          value={s.temperature}
          min={0}
          max={2}
          onChange={(n) => set({ temperature: n })}
        />
        <NumField
          label="Daily token limit"
          value={s.daily_token_budget}
          min={0}
          help="0 means no limit."
          onChange={(n) => set({ daily_token_budget: n })}
        />
        <div className="col-span-2">
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">Language (optional)</span>
            <input
              value={s.language ?? ""}
              onChange={(e) => set({ language: e.target.value || null })}
              className="h-9 rounded-(--radius-control) border border-border bg-(--input-bg) px-3"
              placeholder="English"
            />
          </label>
        </div>
      </div>
      <TagInput
        label="Banned phrases"
        values={s.banned_phrases}
        onChange={(v) => set({ banned_phrases: v })}
      />
      <TagInput label="Spam words" values={s.spam_words} onChange={(v) => set({ spam_words: v })} />
      <Select
        label="When a spam word is found"
        value={s.spam_check}
        onChange={(v) => set({ spam_check: v as S["SpamCheck"] })}
        options={[
          { value: "warn", label: "Warn me" },
          { value: "fail", label: "Reject the text" },
        ]}
      />
      {models.length > 0 && (
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1 text-sm font-medium">
            Prices per million tokens (for the cost view)
          </legend>
          {models.map((m) => (
            <div key={m} className="grid grid-cols-3 items-end gap-2">
              <span className="truncate text-sm">{m}</span>
              <NumField
                label="Input"
                value={price(m).input}
                min={0}
                onChange={(n) => set({ prices: { ...s.prices, [m]: { ...price(m), input: n } } })}
              />
              <NumField
                label="Output"
                value={price(m).output}
                min={0}
                onChange={(n) => set({ prices: { ...s.prices, [m]: { ...price(m), output: n } } })}
              />
            </div>
          ))}
        </fieldset>
      )}
      <div>
        <Button
          onClick={async () => {
            try {
              await http.patch("/v1/generator/settings", { body: s });
              toast.success("Saved.");
              void qc.invalidateQueries({ queryKey: ["generator"] });
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

function MappingRules({ s: initial }: { s: S["LeadSettings"] }) {
  const qc = useQueryClient();
  const [s, setS] = useState(initial);
  useEffect(() => setS(initial), [initial]);
  return (
    <Card className="flex max-w-2xl flex-col gap-3">
      <h2 className="font-semibold">CSV column mapping</h2>
      <Check2
        label="Use AI to match the columns of a CSV file"
        checked={s.ai_mapping_enabled}
        onChange={(v) => setS({ ...s, ai_mapping_enabled: v })}
      />
      <Check2
        label="Hide emails and phone numbers from the model"
        checked={s.mapping_mask_pii}
        onChange={(v) => setS({ ...s, mapping_mask_pii: v })}
      />
      <Check2
        label="Remember mappings for files with the same columns"
        checked={s.mapping_templates_enabled}
        onChange={(v) => setS({ ...s, mapping_templates_enabled: v })}
      />
      <NumField
        label="Ask me to check a column when confidence is below"
        value={s.mapping_min_confidence}
        min={0}
        max={1}
        onChange={(n) => setS({ ...s, mapping_min_confidence: n })}
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

function Usage() {
  const u = useQuery({
    queryKey: ["generator", "usage", "page"],
    queryFn: () => http.get<S["UsageReply"]>("/v1/generator/usage"),
  });
  if (!u.data) return null;
  return (
    <Card className="flex flex-col gap-3">
      <h2 className="font-semibold">Usage, last 30 days</h2>
      <p className="text-muted">
        {fmtInt(u.data.total.input_tokens + u.data.total.output_tokens)} tokens in{" "}
        {fmtInt(u.data.total.calls)} calls ({fmtInt(u.data.total.cached_tokens)} from the cache)
        {u.data.total.cost_usd != null ? `, about $${u.data.total.cost_usd.toFixed(2)}` : ""}.
      </p>
      {u.data.by_model.length > 0 && (
        <Table>
          <Head cols={["Model", "Calls", "Tokens", "Cost"]} />
          <tbody>
            {u.data.by_model.map((m) => (
              <tr key={m.key} className="border-t border-border">
                <td className={TD}>{m.key}</td>
                <td className={TD}>{fmtInt(m.calls)}</td>
                <td className={TD}>{fmtInt(m.input_tokens + m.output_tokens)}</td>
                <td className={TD}>{m.cost_usd != null ? `$${m.cost_usd.toFixed(2)}` : ""}</td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
    </Card>
  );
}
