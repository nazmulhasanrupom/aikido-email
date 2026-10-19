import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Field,
  Head,
  Modal,
  Notice,
  Pill,
  Select,
  Skeleton,
  Table,
  TD,
  TextArea,
} from "@/components/ui";
import { fmtDateTime } from "@/lib/format";
import { say } from "@/lib/words";
import { ImportModal } from "./ImportModal";

/* ---------- Lists ---------- */
export function ListsTab({ onOpen }: { onOpen: (id: string) => void }) {
  const qc = useQueryClient();
  const lists = useQuery(q.lists);
  const [edit, setEdit] = useState<S["LeadList"] | "new" | null>(null);
  const [del, setDel] = useState<S["LeadList"] | null>(null);
  const [name, setName] = useState("");
  const [desc, setDesc] = useState("");
  const save = useMutation({
    mutationFn: () =>
      edit === "new" || !edit
        ? http.post("/v1/leads/lists", { body: { name, description: desc || null } })
        : http.patch("/v1/leads/lists/{id}", {
            path: { id: edit.id },
            body: { name, description: desc || null },
          }),
    onSuccess: (made) => {
      const wasNew = edit === "new";
      setEdit(null);
      void qc.invalidateQueries({ queryKey: q.lists.queryKey });
      if (wasNew && made && typeof made === "object" && "id" in made) onOpen(String(made.id));
    },
  });
  const open = (l: S["LeadList"] | "new") => {
    setEdit(l);
    setName(l === "new" ? "" : l.name);
    setDesc(l === "new" ? "" : (l.description ?? ""));
  };
  return (
    <div className="flex flex-col gap-4">
      <div>
        <Button onClick={() => open("new")}>New list</Button>
      </div>
      {lists.isPending && <Skeleton rows={3} />}
      {lists.isError && <ErrorNote error={lists.error} />}
      {lists.data?.length === 0 && (
        <EmptyState text="A list holds its own rows. Make one, open it and import a CSV with any columns." />
      )}
      {lists.data && lists.data.length > 0 && (
        <Table>
          <Head cols={["Name", "Rows", "Leads", "Description", ""]} />
          <tbody>
            {lists.data.map((l) => (
              <tr key={l.id} className="border-t border-border">
                <td className={`${TD} font-medium`}>
                  <button
                    type="button"
                    className="text-left hover:text-accent hover:underline"
                    onClick={() => onOpen(l.id)}
                  >
                    {l.name}
                  </button>
                </td>
                <td className={TD}>{l.row_count}</td>
                <td className={TD}>{l.member_count}</td>
                <td className={TD}>{l.description}</td>
                <td className={TD}>
                  <Button variant="ghost" className="h-7" onClick={() => onOpen(l.id)}>
                    Open
                  </Button>
                  <Button variant="ghost" className="h-7" onClick={() => open(l)}>
                    Rename
                  </Button>
                  <Button variant="ghost" className="h-7" onClick={() => setDel(l)}>
                    Delete
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      <Modal
        open={!!edit}
        onClose={() => setEdit(null)}
        size="sm"
        title={edit === "new" ? "New list" : "Edit list"}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEdit(null)}>
              Cancel
            </Button>
            <Button busy={save.isPending} disabled={!name.trim()} onClick={() => save.mutate()}>
              Save
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3">
          <Field label="Name" value={name} onChange={(e) => setName(e.target.value)} />
          <TextArea
            label="Description (optional)"
            value={desc}
            onChange={(e) => setDesc(e.target.value)}
          />
          {save.isError && <ErrorNote error={save.error} />}
        </div>
      </Modal>
      <ConfirmDialog
        open={!!del}
        onClose={() => setDel(null)}
        title="Delete this list?"
        text="Its rows are deleted. Leads stay."
        danger
        confirmLabel="Delete list"
        onConfirm={async () => {
          if (del) {
            await http.delEmpty("/v1/leads/lists/{id}", { path: { id: del.id } });
            await qc.invalidateQueries({ queryKey: q.lists.queryKey });
          }
        }}
      />
    </div>
  );
}

/* ---------- Custom fields ---------- */
export function FieldsTab() {
  const qc = useQueryClient();
  const fields = useQuery(q.fields);
  const [edit, setEdit] = useState<S["LeadField"] | "new" | null>(null);
  const [del, setDel] = useState<S["LeadField"] | null>(null);
  const [f, setF] = useState({ key: "", label: "", kind: "text", fallback: "" });
  const save = useMutation({
    mutationFn: () =>
      edit === "new" || !edit
        ? http.post("/v1/leads/fields", {
            body: {
              key: f.key,
              label: f.label || null,
              kind: f.kind,
              fallback: f.fallback || null,
            },
          })
        : http.patch("/v1/leads/fields/{key}", {
            path: { key: edit.key },
            body: { label: f.label, fallback: f.fallback || null },
          }),
    onSuccess: () => {
      setEdit(null);
      void qc.invalidateQueries({ queryKey: q.fields.queryKey });
    },
  });
  const open = (x: S["LeadField"] | "new") => {
    setEdit(x);
    setF(
      x === "new"
        ? { key: "", label: "", kind: "text", fallback: "" }
        : { key: x.key, label: x.label, kind: x.kind, fallback: x.fallback ?? "" },
    );
  };
  const delError = useMutation({
    mutationFn: async (k: string) => {
      await http.del("/v1/leads/fields/{key}", { path: { key: k } });
      await qc.invalidateQueries({ queryKey: q.fields.queryKey });
    },
  });
  return (
    <div className="flex flex-col gap-4">
      <div>
        <Button onClick={() => open("new")}>New field</Button>
      </div>
      {fields.isPending && <Skeleton rows={3} />}
      {fields.data?.length === 0 && (
        <EmptyState text="Custom fields hold extra facts about a lead, such as the industry. Use them in emails as {{key}}." />
      )}
      {fields.data && fields.data.length > 0 && (
        <Table>
          <Head cols={["Key", "Label", "Kind", "If empty", ""]} />
          <tbody>
            {fields.data.map((x) => (
              <tr key={x.key} className="border-t border-border">
                <td className={`${TD} font-mono text-xs`}>{`{{${x.key}}}`}</td>
                <td className={TD}>{x.label}</td>
                <td className={TD}>{x.kind}</td>
                <td className={TD}>{x.fallback}</td>
                <td className={TD}>
                  <Button variant="ghost" className="h-7" onClick={() => open(x)}>
                    Edit
                  </Button>
                  <Button
                    variant="ghost"
                    className="h-7"
                    onClick={() => {
                      delError.reset();
                      setDel(x);
                    }}
                  >
                    Delete
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      <Modal
        open={!!edit}
        onClose={() => setEdit(null)}
        size="sm"
        title={edit === "new" ? "New field" : "Edit field"}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEdit(null)}>
              Cancel
            </Button>
            <Button busy={save.isPending} disabled={!f.key.trim()} onClick={() => save.mutate()}>
              Save
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3">
          <Field
            label="Key"
            value={f.key}
            disabled={edit !== "new"}
            onChange={(e) => setF({ ...f, key: e.target.value })}
            help="Letters, numbers and underscores. You write it as {{key}} in emails."
          />
          <Field
            label="Label"
            value={f.label}
            onChange={(e) => setF({ ...f, label: e.target.value })}
          />
          <Select
            label="Kind"
            value={f.kind}
            disabled={edit !== "new"}
            onChange={(kind) => setF({ ...f, kind })}
            options={["text", "number", "url", "date"].map((k) => ({ value: k, label: k }))}
          />
          <Field
            label="If empty, use"
            value={f.fallback}
            onChange={(e) => setF({ ...f, fallback: e.target.value })}
          />
          {save.isError && <ErrorNote error={save.error} />}
        </div>
      </Modal>
      <Modal
        open={!!del}
        onClose={() => setDel(null)}
        size="sm"
        title="Delete this field?"
        footer={
          <>
            <Button variant="secondary" onClick={() => setDel(null)}>
              Cancel
            </Button>
            <Button
              variant="danger"
              busy={delError.isPending}
              onClick={async () => {
                if (del) {
                  try {
                    await delError.mutateAsync(del.key);
                    setDel(null);
                  } catch {
                    /* shown below */
                  }
                }
              }}
            >
              Delete field
            </Button>
          </>
        }
      >
        <p>This removes the value from every lead.</p>
        {delError.isError && (
          <div className="mt-3">
            <Notice tone="danger">
              {delError.error instanceof ApiProblem && delError.error.code === "field_in_use"
                ? `This field is used in: ${delError.error.message.replace(/^.*used in:?/i, "")}. Remove it there first.`
                : messageOf(delError.error)}
            </Notice>
          </div>
        )}
      </Modal>
    </div>
  );
}

/* ---------- Imports ---------- */
export function ImportsTab() {
  const qc = useQueryClient();
  const [adding, setAdding] = useState(false);
  const [open, setOpen] = useState<S["Import"] | null>(null);
  const list = useInfiniteQuery({
    queryKey: ["leads", "imports"],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      http.get<S["ListImportsReply"]>("/v1/leads/imports", {
        query: { limit: 25, cursor: pageParam },
      }),
    getNextPageParam: (l) => l.next_cursor ?? undefined,
    refetchInterval: (query) =>
      query.state.data?.pages.some((p) =>
        p.imports.some((i) => i.status === "running" || i.status === "pending"),
      )
        ? 2000
        : false,
  });
  const rows = list.data?.pages.flatMap((p) => p.imports) ?? [];
  return (
    <div className="flex flex-col gap-4">
      <div>
        <Button onClick={() => setAdding(true)}>Import CSV</Button>
      </div>
      {list.isPending && <Skeleton rows={3} />}
      {list.isError && <ErrorNote error={list.error} />}
      {list.data && rows.length === 0 && <EmptyState text="Your imports will show here." />}
      {rows.length > 0 && (
        <Table>
          <Head
            cols={["File", "Status", "New", "Updated", "Skipped", "Not valid", "Started", ""]}
          />
          <tbody>
            {rows.map((i) => (
              <tr key={i.id} className="border-t border-border">
                <td className={TD}>{i.file_name}</td>
                <td className={TD}>
                  <Pill
                    tone={
                      i.status === "done" ? "success" : i.status === "failed" ? "danger" : "neutral"
                    }
                  >
                    {say("importStatus", i.status)}
                  </Pill>
                  {i.status === "running" && i.total_rows > 0 && (
                    <div className="mt-1 h-1.5 w-24 overflow-hidden rounded-full bg-surface-2">
                      <div
                        className="h-full bg-accent"
                        style={{ width: `${Math.round((i.rows_done / i.total_rows) * 100)}%` }}
                      />
                    </div>
                  )}
                </td>
                <td className={TD}>{i.created}</td>
                <td className={TD}>{i.updated}</td>
                <td className={TD}>{i.skipped}</td>
                <td className={TD}>{i.invalid}</td>
                <td className={TD}>{fmtDateTime(i.created_at)}</td>
                <td className={TD}>
                  <Button variant="ghost" className="h-7" onClick={() => setOpen(i)}>
                    Details
                  </Button>
                  {(i.status === "running" ||
                    i.status === "pending" ||
                    i.status === "uploaded") && (
                    <Button
                      variant="ghost"
                      className="h-7"
                      onClick={async () => {
                        try {
                          await http.post("/v1/leads/imports/{id}/cancel", { path: { id: i.id } });
                          void qc.invalidateQueries({ queryKey: ["leads", "imports"] });
                        } catch (e) {
                          toast.error(messageOf(e));
                        }
                      }}
                    >
                      Cancel
                    </Button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      {list.hasNextPage && (
        <div>
          <Button variant="secondary" onClick={() => list.fetchNextPage()}>
            Load more
          </Button>
        </div>
      )}
      <ImportModal
        open={adding}
        onClose={() => {
          setAdding(false);
          void qc.invalidateQueries({ queryKey: ["leads", "imports"] });
        }}
      />
      <Modal
        open={!!open}
        onClose={() => setOpen(null)}
        title={open?.file_name ?? "Import"}
        size="md"
      >
        {open && (
          <div className="flex flex-col gap-3 text-sm">
            {open.error && <Notice tone="danger">{open.error}</Notice>}
            <p>
              {open.created} new, {open.updated} updated, {open.skipped} skipped, {open.invalid} not
              valid.
            </p>
            {Array.isArray(open.errors) && open.errors.length > 0 ? (
              <ul className="max-h-64 overflow-auto rounded border border-border p-2">
                {(open.errors as { row?: number; reason?: string; message?: string }[]).map(
                  (e, n) => (
                    <li key={`${e.row}-${n}`}>
                      Row {e.row}: {e.reason ?? e.message}
                    </li>
                  ),
                )}
              </ul>
            ) : (
              <p className="text-muted">No error rows.</p>
            )}
          </div>
        )}
      </Modal>
    </div>
  );
}

/* ---------- Blocklist ---------- */
export function BlocklistTab() {
  const qc = useQueryClient();
  const list = useQuery({
    queryKey: ["sending", "suppressions"],
    queryFn: () =>
      http.get<S["Suppression"][]>("/v1/sending/suppressions", { query: { limit: 200 } }),
  });
  const [adding, setAdding] = useState(false);
  const [value, setValue] = useState("");
  const [del, setDel] = useState<S["Suppression"] | null>(null);
  const add = useMutation({
    mutationFn: () =>
      http.post("/v1/sending/suppressions", {
        body: {
          kind: value.includes("@") ? "email" : "domain",
          value: value.trim(),
          reason: "manual",
        },
      }),
    onSuccess: () => {
      setAdding(false);
      setValue("");
      void qc.invalidateQueries({ queryKey: ["sending", "suppressions"] });
    },
  });
  return (
    <div className="flex flex-col gap-4">
      <div>
        <Button onClick={() => setAdding(true)}>Add to blocklist</Button>
      </div>
      {list.isPending && <Skeleton rows={3} />}
      {list.isError && <ErrorNote error={list.error} />}
      {list.data?.length === 0 && (
        <EmptyState text="Addresses and domains we never email appear here. Replies, unsubscribes and bounces add them for you." />
      )}
      {list.data && list.data.length > 0 && (
        <Table>
          <Head cols={["Value", "Type", "Reason", "Added", ""]} />
          <tbody>
            {list.data.map((s) => (
              <tr key={`${s.kind}:${s.value}`} className="border-t border-border">
                <td className={TD}>{s.value}</td>
                <td className={TD}>{s.kind === "email" ? "Email" : "Domain"}</td>
                <td className={TD}>{say("suppression", s.reason)}</td>
                <td className={TD}>{fmtDateTime(s.created_at)}</td>
                <td className={TD}>
                  <Button variant="ghost" className="h-7" onClick={() => setDel(s)}>
                    Remove
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      <Modal
        open={adding}
        onClose={() => setAdding(false)}
        size="sm"
        title="Add to blocklist"
        footer={
          <>
            <Button variant="secondary" onClick={() => setAdding(false)}>
              Cancel
            </Button>
            <Button busy={add.isPending} disabled={!value.trim()} onClick={() => add.mutate()}>
              Add
            </Button>
          </>
        }
      >
        <Field
          label="Email address or domain"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          help="For example ana@acme.com or acme.com."
        />
        {add.isError && (
          <div className="mt-3">
            <ErrorNote error={add.error} />
          </div>
        )}
      </Modal>
      <ConfirmDialog
        open={!!del}
        onClose={() => setDel(null)}
        title="Remove from the blocklist?"
        text="We may email this address again."
        confirmLabel="Remove"
        danger={false}
        onConfirm={async () => {
          if (del) {
            await http.delEmpty("/v1/sending/suppressions", {
              query: { kind: del.kind, value: del.value },
            });
            await qc.invalidateQueries({ queryKey: ["sending", "suppressions"] });
          }
        }}
      />
    </div>
  );
}
