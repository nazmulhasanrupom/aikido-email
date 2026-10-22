import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { BookOpen } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { PasswordField } from "@/components/PasswordField";
import {
  Button,
  Card,
  CopyButton,
  ErrorNote,
  Field,
  Head,
  Notice,
  Pill,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";

/** The groups of keys, in page order. `redirect` is the key of `redirect_uris` in the ui config. */
const GROUPS = [
  {
    id: "google",
    title: "Google (Gmail)",
    text: "Lets people connect a Gmail or Google Workspace inbox with one click.",
    guide: "google",
    redirect: "gmail",
  },
  {
    id: "microsoft",
    title: "Microsoft (Outlook)",
    text: "Lets people connect an Outlook or Microsoft 365 inbox with one click.",
    guide: "microsoft",
    redirect: "microsoft",
  },
] as const;

/** Settings → Environment: the keys that used to live in `.env`. A saved value works at once. */
export function EnvironmentForm({ ws }: { ws: string }) {
  const env = useQuery(q.environment);
  if (env.isPending) return <Skeleton rows={4} />;
  if (env.isError) return <ErrorNote error={env.error} onRetry={() => env.refetch()} />;
  return (
    <div className="flex flex-col gap-5">
      <Notice>
        Type your keys here and save. They work at once, with no restart. A key set in{" "}
        <code className="font-mono">.env</code> on the server wins and is locked here.
      </Notice>
      {GROUPS.map((g) => (
        <GroupCard
          key={g.id}
          ws={ws}
          group={g}
          items={env.data.items.filter((i) => i.group === g.id)}
        />
      ))}
      <Card className="flex flex-col gap-2">
        <h2 className="font-semibold">Other keys</h2>
        <p className="text-sm text-muted">
          AI models and the Reoon email check have their own screens:{" "}
          <Link to="/w/$ws/settings/integrations" params={{ ws }} className="text-accent underline">
            Settings → Integrations
          </Link>
          .
        </p>
      </Card>
      <ReadOnly ws={ws} rows={env.data.read_only} />
    </div>
  );
}

function GroupCard({
  ws,
  group,
  items,
}: {
  ws: string;
  group: (typeof GROUPS)[number];
  items: S["EnvSettingItem"][];
}) {
  const qc = useQueryClient();
  const config = useQuery(q.config).data;
  const redirect = config?.redirect_uris[group.redirect] ?? null;
  // Typed changes. `null` means remove the saved value. A key that is not here is unchanged.
  const [draft, setDraft] = useState<Record<string, string | null>>({});
  const [replacing, setReplacing] = useState<Record<string, boolean>>({});
  const [test, setTest] = useState<S["EnvTestReply"] | null>(null);

  const save = useMutation({
    mutationFn: () => {
      const values: Record<string, string | null> = {};
      for (const [key, v] of Object.entries(draft)) values[key] = v === "" ? null : v;
      return http.put<S["EnvSettingsReply"]>("/v1/settings/environment", { body: { values } });
    },
    onSuccess: (reply) => {
      qc.setQueryData(q.environment.queryKey, reply);
      setDraft({});
      setReplacing({});
      setTest(null);
      toast.success("Saved.");
      // the Connect Gmail / Outlook buttons follow at once
      void qc.invalidateQueries({ queryKey: q.config.queryKey });
      void qc.invalidateQueries({ queryKey: q.onboarding.queryKey });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const check = useMutation({
    mutationFn: () =>
      http.post<S["EnvTestReply"]>("/v1/settings/environment/test/{group}", {
        path: { group: group.id },
      }),
    onSuccess: setTest,
    onError: (e) => setTest({ ok: false, message: messageOf(e) }),
  });

  const changed = Object.keys(draft).length > 0;
  return (
    <Card className="flex flex-col gap-4" aria-label={group.title}>
      <div className="flex flex-col gap-1">
        <h2 className="font-semibold">{group.title}</h2>
        <p className="text-sm text-muted">{group.text}</p>
        <p className="text-sm">
          <Link
            to="/w/$ws/guide"
            params={{ ws }}
            hash={group.guide}
            className="inline-flex items-center gap-1.5 text-accent underline"
          >
            <BookOpen size={14} aria-hidden="true" /> How to get this
          </Link>
        </p>
      </div>
      <div className="flex flex-col gap-1">
        <span className="text-sm font-medium">Redirect address</span>
        <div className="flex items-center gap-2">
          <code className="min-w-0 flex-1 truncate rounded bg-surface-2 px-2 py-1 font-mono text-xs">
            {redirect ?? "Set AIKIDO_PUBLIC_URL in .env first"}
          </code>
          {redirect && <CopyButton text={redirect} />}
        </div>
        <p className="text-xs text-muted">Enter this address in the console of the provider.</p>
      </div>
      {items.map((item) => (
        <Row
          key={item.key}
          item={item}
          draft={draft[item.key]}
          replacing={!!replacing[item.key]}
          onChange={(v) => setDraft((d) => ({ ...d, [item.key]: v }))}
          onReplace={() => setReplacing((r) => ({ ...r, [item.key]: true }))}
        />
      ))}
      {test && <Notice tone={test.ok ? "success" : "warning"}>{test.message}</Notice>}
      <div className="flex gap-2">
        <Button busy={save.isPending} disabled={!changed} onClick={() => save.mutate()}>
          Save
        </Button>
        <Button variant="secondary" busy={check.isPending} onClick={() => check.mutate()}>
          Test
        </Button>
      </div>
    </Card>
  );
}

function Row({
  item,
  draft,
  replacing,
  onChange,
  onReplace,
}: {
  item: S["EnvSettingItem"];
  draft: string | null | undefined;
  replacing: boolean;
  onChange: (v: string | null) => void;
  onReplace: () => void;
}) {
  const fromEnv = item.source === "env";
  const lockedPill = fromEnv ? <Pill tone="info">Set in .env</Pill> : null;
  if (item.secret && item.is_set && !replacing) {
    return (
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-sm font-medium">{item.label}</span>
        <span className="text-sm text-muted">
          Saved{item.last4 ? ` (ends in ${item.last4})` : ""}
        </span>
        {lockedPill}
        {!fromEnv && (
          <Button variant="secondary" className="h-8" onClick={onReplace}>
            Replace
          </Button>
        )}
      </div>
    );
  }
  const value = draft ?? (item.secret ? "" : (item.value ?? ""));
  const Input = item.secret ? PasswordField : Field;
  return (
    <div className="flex flex-col gap-1.5">
      <Input
        label={item.label}
        value={value}
        disabled={fromEnv}
        autoComplete="off"
        spellCheck={false}
        placeholder={item.default ? `Default: ${item.default}` : undefined}
        help={item.help}
        onChange={(e) => onChange(e.target.value)}
      />
      {lockedPill}
    </div>
  );
}

function ReadOnly({ ws, rows }: { ws: string; rows: S["EnvReadOnlyItem"][] }) {
  return (
    <Card className="flex flex-col gap-3">
      <div>
        <h2 className="font-semibold">Server settings (read only)</h2>
        <p className="text-sm text-muted">
          Set in <code className="font-mono">.env</code> on the server. The server needs these
          before it can read the database, so they cannot change here. See the{" "}
          <Link to="/w/$ws/guide" params={{ ws }} hash="url" className="text-accent underline">
            Guide
          </Link>
          .
        </p>
      </div>
      <Table>
        <Head cols={["Name", "Value"]} />
        <tbody>
          {rows.map((r) => (
            <tr key={r.key} className="border-t border-border">
              <td className={`${TD} font-mono text-xs`}>{r.key}</td>
              <td className={`${TD} text-sm`}>
                {r.secret ? (
                  <span className="text-muted">Hidden</span>
                ) : r.value ? (
                  <span className="font-mono text-xs">{r.value}</span>
                ) : (
                  <span className="text-muted">Not set</span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </Table>
    </Card>
  );
}
