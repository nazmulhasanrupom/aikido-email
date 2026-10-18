import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Columns3, Plus } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { authStateQuery } from "@/api/auth";
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
import { Button, CopyButton, ErrorNote, Modal, Select } from "@/components/ui";
import { keyOrFallback, VALID_KEY } from "@/lib/fieldKey";

/** One column of a lead table. A custom column has `field` set. */
export type GridCol = { id: string; label: string; field?: S["LeadField"] };

type LeadLike = { id: string } & Record<string, unknown>;

export const BUILTIN_EDITABLE = new Set([
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
]);

/* ---------- column preferences, saved per user ---------- */

type Pref = { order?: string[]; hidden?: string[] };

/** Show, hide and order columns. Saved in `users.preferences.tables.<tableId>`. */
export function useColumns(tableId: string, base: GridCol[]) {
  const qc = useQueryClient();
  const fields = useQuery(q.fields);
  const auth = useQuery(authStateQuery);
  const saved = (
    auth.data?.user?.preferences as { tables?: Record<string, Pref> } | undefined
  )?.tables?.[tableId];
  const all: GridCol[] = [
    ...base,
    ...(fields.data ?? []).map((f) => ({ id: `custom:${f.key}`, label: f.label || f.key, field: f })),
  ];
  const order = saved?.order ?? [];
  const sorted = [...all].sort((a, b) => {
    const ia = order.indexOf(a.id);
    const ib = order.indexOf(b.id);
    return (ia < 0 ? 1e6 + all.indexOf(a) : ia) - (ib < 0 ? 1e6 + all.indexOf(b) : ib);
  });
  const hidden = new Set(saved?.hidden ?? []);
  const save = async (next: Pref) => {
    try {
      await http.patch("/v1/users/me", {
        body: { preferences: { tables: { [tableId]: next } } },
      });
      await qc.invalidateQueries({ queryKey: authStateQuery.queryKey });
    } catch (e) {
      toast.error(messageOf(e));
    }
  };
  return {
    all: sorted,
    visible: sorted.filter((c) => !hidden.has(c.id)),
    hidden,
    toggle: (id: string) => {
      const h = new Set(hidden);
      if (h.has(id)) h.delete(id);
      else h.add(id);
      return save({ order: sorted.map((c) => c.id), hidden: [...h] });
    },
    move: (id: string, by: -1 | 1) => {
      const ids = sorted.map((c) => c.id);
      const i = ids.indexOf(id);
      const j = i + by;
      if (i < 0 || j < 0 || j >= ids.length) return Promise.resolve();
      [ids[i], ids[j]] = [ids[j] as string, ids[i] as string];
      return save({ order: ids, hidden: [...hidden] });
    },
  };
}

export function ColumnsMenu({ cols }: { cols: ReturnType<typeof useColumns> }) {
  return (
    <MenuRoot>
      <MenuTrigger asChild>
        <Button variant="secondary">
          <Columns3 size={16} /> Columns
        </Button>
      </MenuTrigger>
      <MenuContent align="end" className="max-h-96 overflow-auto">
        <MenuLabel>Show, hide and move columns</MenuLabel>
        {cols.all.map((c, i) => (
          <div key={c.id} className="flex items-center gap-1 px-1">
            <MenuItem
              className="flex-1"
              onSelect={(e) => {
                e.preventDefault();
                void cols.toggle(c.id);
              }}
            >
              <input
                type="checkbox"
                readOnly
                tabIndex={-1}
                checked={!cols.hidden.has(c.id)}
                aria-label={`Show ${c.label}`}
              />
              {c.label}
            </MenuItem>
            <button
              type="button"
              aria-label={`Move ${c.label} left`}
              disabled={i === 0}
              className="rounded px-1 hover:bg-surface-2 disabled:opacity-30"
              onClick={() => void cols.move(c.id, -1)}
            >
              ↑
            </button>
            <button
              type="button"
              aria-label={`Move ${c.label} right`}
              disabled={i === cols.all.length - 1}
              className="rounded px-1 hover:bg-surface-2 disabled:opacity-30"
              onClick={() => void cols.move(c.id, 1)}
            >
              ↓
            </button>
          </div>
        ))}
        <MenuSeparator />
      </MenuContent>
    </MenuRoot>
  );
}

/* ---------- inline edit ---------- */

export function cellValue(lead: Record<string, unknown>, col: GridCol): string {
  if (col.field) {
    const v = (lead.custom_fields as Record<string, unknown> | null | undefined)?.[col.field.key];
    return v == null ? "" : String(v);
  }
  const v = lead[col.id];
  return v == null ? "" : String(v);
}

const INPUT_TYPE = { text: "text", number: "number", url: "url", date: "date" } as const;

/** A table cell. Double-click (or Enter) to edit. Enter saves, Esc cancels. */
export function EditableCell({
  lead,
  col,
  inCampaign,
  locked,
}: {
  lead: LeadLike;
  col: GridCol;
  inCampaign?: boolean;
  locked?: boolean;
}) {
  const qc = useQueryClient();
  const value = cellValue(lead, col);
  const editable = !!col.field || BUILTIN_EDITABLE.has(col.id);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLInputElement>(null);
  const kind = col.field?.kind ?? (col.id === "email" ? "text" : "text");
  useEffect(() => {
    if (editing) ref.current?.focus();
  }, [editing]);

  const save = useMutation({
    mutationFn: (v: string) => {
      const body = col.field
        ? { custom_fields: { [col.field.key]: v === "" ? null : v } }
        : { [col.id]: v === "" && col.id !== "email" ? null : v };
      return http.patch("/v1/leads/{id}", { path: { id: lead.id }, body });
    },
    onSuccess: () => {
      setEditing(false);
      setError(null);
      if (inCampaign) toast.success("Saved for this lead in all campaigns.");
      void qc.invalidateQueries({ queryKey: ["leads"] });
      void qc.invalidateQueries({ queryKey: ["campaigns"] });
    },
    onError: (e) =>
      setError(
        e instanceof ApiProblem && e.code === "email_locked"
          ? "Stop the sequence to change the email."
          : messageOf(e),
      ),
  });

  if (!editable) return <>{value}</>;
  if (editing) {
    return (
      <div>
        <input
          ref={ref}
          type={INPUT_TYPE[kind]}
          aria-label={`Edit ${col.label}`}
          className="h-8 w-full min-w-28 rounded border border-accent bg-surface px-2 text-[13px]"
          value={draft}
          disabled={save.isPending}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              if (draft === value) setEditing(false);
              else save.mutate(draft);
            }
            if (e.key === "Escape") {
              setEditing(false);
              setError(null);
            }
          }}
          onBlur={() => {
            if (!save.isPending && !error) setEditing(false);
          }}
        />
        {error && (
          <p role="alert" className="mt-1 text-xs text-danger">
            {error}
          </p>
        )}
      </div>
    );
  }
  return (
    <div
      role="gridcell"
      tabIndex={0}
      title={
        locked && col.id === "email" ? "Stop the sequence to change the email." : "Double-click to edit"
      }
      className="min-h-6 cursor-text rounded px-1 outline-none focus-visible:ring-2 focus-visible:ring-accent"
      onDoubleClick={() => {
        if (locked && col.id === "email") return;
        setDraft(value);
        setEditing(true);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" && !(locked && col.id === "email")) {
          setDraft(value);
          setEditing(true);
        }
      }}
    >
      {value}
    </div>
  );
}

/* ---------- add a column ---------- */

const KINDS = [
  { value: "text", label: "Text" },
  { value: "number", label: "Number" },
  { value: "url", label: "Link" },
  { value: "date", label: "Date" },
];

/** The "+" at the end of a table header. Makes a custom field for the whole system. */
export function AddColumn() {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [kind, setKind] = useState("text");
  const [fallback, setFallback] = useState("");
  const fields = useQuery(q.fields);
  const key = keyOrFallback(name, (fields.data?.length ?? 0) + 1);
  const exists = fields.data?.some((f) => f.key === key);
  const create = useMutation({
    mutationFn: () =>
      http.post("/v1/leads/fields", {
        body: { key, label: name.trim(), kind: kind as S["FieldKind"], fallback: fallback || null },
      }),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: q.fields.queryKey });
      toast.success(`New column. Use it in emails as {{${key}}}.`);
      setOpen(false);
      setName("");
      setFallback("");
      setKind("text");
    },
  });
  return (
    <>
      <button
        type="button"
        aria-label="Add column"
        className="rounded p-1 hover:bg-surface-2"
        onClick={() => setOpen(true)}
      >
        <Plus size={16} />
      </button>
      <Modal
        open={open}
        onClose={() => setOpen(false)}
        size="sm"
        title="Add a column"
        footer={
          <>
            <Button variant="secondary" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button
              busy={create.isPending}
              disabled={!name.trim() || !!exists || !VALID_KEY.test(key)}
              onClick={() => create.mutate()}
            >
              Add column
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3">
          <label className="flex flex-col gap-1 text-sm font-medium">
            Column name
            <input
              className="h-9 rounded border border-border bg-surface-2 px-2 text-sm font-normal"
              value={name}
              onChange={(e) => {
                setName(e.target.value);
                create.reset();
              }}
            />
          </label>
          <Select label="Type" value={kind} options={KINDS} onChange={setKind} />
          <label className="flex flex-col gap-1 text-sm font-medium">
            Value when empty (optional)
            <input
              className="h-9 rounded border border-border bg-surface-2 px-2 text-sm font-normal"
              value={fallback}
              onChange={(e) => setFallback(e.target.value)}
            />
          </label>
          {name.trim() && (
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <span className="text-muted">Use it in emails as</span>
              <code>{`{{${key}}}`}</code>
              <CopyButton text={`{{${key}}}`} />
            </div>
          )}
          {exists && <p className="text-xs text-danger">A column with this key exists. Pick another name.</p>}
          {!VALID_KEY.test(key) && name.trim() && (
            <p className="text-xs text-danger">Use letters and digits.</p>
          )}
          {create.isError && <ErrorNote error={create.error} />}
        </div>
      </Modal>
    </>
  );
}

/* ---------- header menu of a custom column ---------- */

/** Column title. A custom column gets a menu: rename, value when empty, delete. */
export function ColumnHeader({ col }: { col: GridCol }) {
  const qc = useQueryClient();
  const f = col.field;
  const [mode, setMode] = useState<null | "rename" | "fallback" | "delete">(null);
  const [text, setText] = useState("");
  const done = async () => {
    await qc.invalidateQueries({ queryKey: q.fields.queryKey });
    setMode(null);
  };
  const save = useMutation({
    mutationFn: () =>
      http.patch("/v1/leads/fields/{key}", {
        path: { key: f?.key as string },
        body: mode === "rename" ? { label: text.trim() } : { fallback: text || null },
      }),
    onSuccess: done,
  });
  const del = useMutation({
    mutationFn: () => http.del("/v1/leads/fields/{key}", { path: { key: f?.key as string } }),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["leads"] });
      await done();
    },
  });
  if (!f) return <>{col.label}</>;
  const err = mode === "delete" ? del.error : save.error;
  return (
    <>
      <MenuRoot>
        <MenuTrigger
          aria-label={`Column menu for ${col.label}`}
          className="rounded px-1 hover:bg-surface-2"
        >
          {col.label} ▾
        </MenuTrigger>
        <MenuContent>
          <MenuItem
            onSelect={() => {
              setText(f.label || f.key);
              setMode("rename");
            }}
          >
            Rename
          </MenuItem>
          <MenuItem
            onSelect={() => {
              setText(f.fallback ?? "");
              setMode("fallback");
            }}
          >
            Set value when empty
          </MenuItem>
          <MenuItem onSelect={() => setMode("delete")}>Delete column</MenuItem>
        </MenuContent>
      </MenuRoot>
      <Modal
        open={mode !== null}
        onClose={() => setMode(null)}
        size="sm"
        title={
          mode === "rename" ? "Rename column" : mode === "fallback" ? "Value when empty" : "Delete column?"
        }
        footer={
          <>
            <Button variant="secondary" onClick={() => setMode(null)}>
              Cancel
            </Button>
            {mode === "delete" ? (
              <Button variant="danger" busy={del.isPending} onClick={() => del.mutate()}>
                Delete column
              </Button>
            ) : (
              <Button busy={save.isPending} disabled={mode === "rename" && !text.trim()} onClick={() => save.mutate()}>
                Save
              </Button>
            )}
          </>
        }
      >
        {mode === "delete" ? (
          <p>
            This removes {`{{${f.key}}}`} and its value from every lead. If an email uses it, the
            server refuses.
          </p>
        ) : (
          <label className="flex flex-col gap-1 text-sm font-medium">
            {mode === "rename" ? "Name (the key stays the same)" : "Value when empty"}
            <input
              className="h-9 rounded border border-border bg-surface-2 px-2 text-sm font-normal"
              value={text}
              onChange={(e) => setText(e.target.value)}
            />
          </label>
        )}
        {err != null && (
          <div className="mt-3">
            <ErrorNote error={err} />
          </div>
        )}
      </Modal>
    </>
  );
}
