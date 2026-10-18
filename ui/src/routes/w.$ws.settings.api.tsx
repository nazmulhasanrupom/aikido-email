import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import {
  Button,
  Card,
  ConfirmDialog,
  CopyButton,
  EmptyState,
  ErrorNote,
  Field,
  Head,
  Modal,
  Notice,
  Select,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";
import { useUser } from "@/features/settings/useUser";
import { fmtAgo, fmtDate } from "@/lib/format";
import { PROBLEM_WORDS } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/settings/api")({ component: Api });

const SCOPES = [
  "admin",
  "accounts",
  "sending",
  "replies",
  "followups",
  "leads",
  "campaigns",
  "generator",
  "integrations",
  "personalization",
];

function Api() {
  const { isAdmin } = useUser();
  const qc = useQueryClient();
  const keys = useQuery({
    queryKey: ["keys"],
    enabled: isAdmin,
    queryFn: () => http.get<S["KeyInfo"][]>("/v1/keys"),
  });
  const [making, setMaking] = useState(false);
  const [scope, setScope] = useState("sending");
  const [name, setName] = useState("");
  const [shown, setShown] = useState<string | null>(null);
  const [revoke, setRevoke] = useState<S["KeyInfo"] | null>(null);
  const [rotate, setRotate] = useState<S["KeyInfo"] | null>(null);
  const create = useMutation({
    mutationFn: () =>
      http.post<S["NewKeyReply"]>("/v1/keys", { body: { scope, name: name || null } }),
    onSuccess: (r) => {
      setMaking(false);
      setName("");
      setShown(r.plain_key);
      void qc.invalidateQueries({ queryKey: ["keys"] });
    },
  });
  const makeMaster = useMutation({
    mutationFn: () =>
      http.post<S["NewKeyReply"]>("/v1/keys", { body: { scope: "admin", name: "Master key" } }),
    onSuccess: (r) => {
      setShown(r.plain_key);
      void qc.invalidateQueries({ queryKey: ["keys"] });
    },
  });
  if (!isAdmin)
    return <Notice tone="warning">Only an owner or an admin can manage API keys.</Notice>;
  const masters = keys.data?.filter((k) => k.scope === "admin" && !k.revoked_at) ?? [];
  const others = keys.data?.filter((k) => k.scope !== "admin") ?? [];
  return (
    <div className="flex flex-col gap-5">
      <Card className="flex flex-col gap-1">
        <p>
          API address: <code className="font-mono">{window.location.origin}</code>
        </p>
        <p>
          <a className="text-accent underline" href="/docs" target="_blank" rel="noreferrer">
            Open the API docs
          </a>
        </p>
      </Card>
      <Card className="flex flex-col gap-3">
        <div>
          <h2 className="font-semibold">Master key</h2>
          <p className="text-sm text-muted">
            One key that works for every action in the API. Send it as{" "}
            <code className="font-mono">Authorization: Bearer &lt;key&gt;</code>. Keep it secret.
          </p>
        </div>
        {keys.isPending && <Skeleton rows={1} />}
        {keys.isError && <ErrorNote error={keys.error} />}
        {masters.map((k) => (
          <div
            key={k.id}
            className="flex flex-wrap items-center gap-3 rounded border border-border p-3"
          >
            <code className="font-mono text-xs">{k.key_prefix}…</code>
            <span className="text-sm text-muted">
              Last used {fmtAgo(k.last_used_at)} · Made {fmtDate(k.created_at)}
            </span>
            <span className="ml-auto flex gap-1">
              <Button variant="ghost" className="h-7" onClick={() => setRotate(k)}>
                Rotate
              </Button>
              <Button variant="ghost" className="h-7" onClick={() => setRevoke(k)}>
                Revoke
              </Button>
            </span>
          </div>
        ))}
        {keys.data && masters.length === 0 && (
          <Notice tone="warning">No master key yet. Make one to use the API.</Notice>
        )}
        {makeMaster.isError && <ErrorNote error={makeMaster.error} />}
        <div>
          <Button busy={makeMaster.isPending} onClick={() => makeMaster.mutate()}>
            {masters.length === 0 ? "Generate master key" : "Generate another master key"}
          </Button>
        </div>
      </Card>
      <div className="flex items-center justify-between">
        <div>
          <h2 className="font-semibold">Section keys (optional)</h2>
          <p className="text-sm text-muted">
            Keys that open only one part of the API. You do not need these.
          </p>
        </div>
        <Button variant="secondary" onClick={() => setMaking(true)}>
          New key
        </Button>
      </div>
      {keys.data && others.length === 0 && <EmptyState text="No section keys." />}
      {others.length > 0 && (
        <Table>
          <Head cols={["Name", "Scope", "Key starts with", "Last used", "Created", ""]} />
          <tbody>
            {others.map((k) => (
              <tr key={k.id} className="border-t border-border">
                <td className={TD}>
                  {k.name}
                  {k.revoked_at ? " (revoked)" : ""}
                </td>
                <td className={TD}>{k.scope}</td>
                <td className={`${TD} font-mono text-xs`}>{k.key_prefix}</td>
                <td className={TD}>{fmtAgo(k.last_used_at)}</td>
                <td className={TD}>{fmtDate(k.created_at)}</td>
                <td className={TD}>
                  {!k.revoked_at && (
                    <>
                      <Button variant="ghost" className="h-7" onClick={() => setRotate(k)}>
                        Rotate
                      </Button>
                      <Button variant="ghost" className="h-7" onClick={() => setRevoke(k)}>
                        Revoke
                      </Button>
                    </>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      <Modal
        open={making}
        onClose={() => setMaking(false)}
        size="sm"
        title="New API key"
        footer={
          <>
            <Button variant="secondary" onClick={() => setMaking(false)}>
              Cancel
            </Button>
            <Button busy={create.isPending} onClick={() => create.mutate()}>
              Make key
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3">
          <Field label="Name (optional)" value={name} onChange={(e) => setName(e.target.value)} />
          <Select
            label="What it may do"
            value={scope}
            onChange={setScope}
            options={SCOPES.filter((x) => x !== "admin").map((s) => ({ value: s, label: s }))}
          />
          {create.isError && <ErrorNote error={create.error} />}
        </div>
      </Modal>
      <Modal
        open={!!shown}
        onClose={() => setShown(null)}
        size="md"
        title="Your new key"
        footer={<Button onClick={() => setShown(null)}>I saved it</Button>}
      >
        <div className="flex flex-col gap-3">
          <Notice tone="warning">You will not see this key again. Copy it now.</Notice>
          <code className="break-all rounded bg-surface-2 p-3 font-mono text-sm">{shown}</code>
          {shown && (
            <div>
              <CopyButton text={shown} />
            </div>
          )}
        </div>
      </Modal>
      <ConfirmDialog
        open={!!revoke}
        onClose={() => setRevoke(null)}
        title="Revoke this key?"
        text="Anything that uses it stops working at once."
        confirmLabel="Revoke key"
        onConfirm={async () => {
          try {
            await http.del("/v1/keys/{id}", { path: { id: (revoke as S["KeyInfo"]).id } });
            await qc.invalidateQueries({ queryKey: ["keys"] });
          } catch (e) {
            throw new Error(
              e instanceof ApiProblem ? (PROBLEM_WORDS[e.code] ?? e.message) : messageOf(e),
            );
          }
        }}
      />
      <ConfirmDialog
        open={!!rotate}
        onClose={() => setRotate(null)}
        danger={false}
        title="Rotate this key?"
        text="You get a new key. The old one keeps working for 24 hours."
        confirmLabel="Rotate key"
        onConfirm={async () => {
          const r = await http.post<S["RotateReply"]>("/v1/keys/{id}/rotate", {
            path: { id: (rotate as S["KeyInfo"]).id },
            body: { grace_hours: 24 },
          });
          setShown(r.plain_key);
          await qc.invalidateQueries({ queryKey: ["keys"] });
        }}
      />
    </div>
  );
}
