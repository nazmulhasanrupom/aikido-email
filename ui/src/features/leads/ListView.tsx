import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, ChevronLeft, ChevronRight, MoreHorizontal } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S, upload } from "@/api/http";
import { q } from "@/api/queries";
import { MenuContent, MenuItem, MenuRoot, MenuTrigger } from "@/components/shell/menu";
import {
  Button,
  ConfirmDialog,
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
  useDebounced,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { fmtInt } from "@/lib/format";
import { ListToCampaign } from "./ImportFlow";

type Row = { id: string; data: Record<string, string>; created_at: string; updated_at: string };
type RowsReply = { columns: string[]; rows: Row[]; total: number; next_cursor?: string | null };
const PAGE_SIZE = 100;
type ImportReply = { added: number; empty: number; columns: string[]; new_columns: string[] };

/**
 * One list and its own rows. The rows came from CSV files as they are, with every column.
 * Editing here changes this list only. "Add to campaign" maps the columns and makes leads.
 */
export function ListView({ id, onBack }: { id: string; onBack: () => void }) {
  const qc = useQueryClient();
  const lists = useQuery(q.lists);
  const list = lists.data?.find((l) => l.id === id);
  const [text, setText] = useState("");
  const find = useDebounced(text.trim(), 300);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [dialog, setDialog] = useState<
    | null
    | "import"
    | "column"
    | "row"
    | "campaign"
    | "delete"
    | { rename: string }
    | { drop: string }
  >(null);
  // The API pages by cursor. The cursors of the pages we have passed are kept, so Previous can go back.
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
  const page = cursors.length - 1;
  const cursor = cursors[page];
  useEffect(() => {
    void find;
    setCursors([undefined]);
  }, [find]);
  const key = ["leads", "lists", id, "rows", find, cursor ?? "first"] as const;
  const rows = useQuery({
    queryKey: key,
    queryFn: () =>
      http.get<RowsReply>("/v1/leads/lists/{id}/rows", {
        path: { id },
        query: { limit: PAGE_SIZE, cursor, q: find },
      }),
    placeholderData: keepPreviousData,
  });
  const data = rows.data?.rows ?? [];
  const columns = rows.data?.columns ?? list?.columns ?? [];
  const total = rows.data?.total ?? 0;
  const nextCursor = rows.data?.next_cursor ?? undefined;
  const pages = Math.max(1, Math.ceil(total / PAGE_SIZE));
  // the last rows of a page were deleted: go back one page
  useEffect(() => {
    if (rows.data && !rows.isPlaceholderData && rows.data.rows.length === 0 && page > 0) {
      setCursors((c) => c.slice(0, -1));
    }
  }, [rows.data, rows.isPlaceholderData, page]);
  const refresh = () => void qc.invalidateQueries({ queryKey: ["leads", "lists"] });

  const saveCell = useMutation({
    mutationFn: ({ row, col, value }: { row: string; col: string; value: string }) =>
      http.patch<Row>("/v1/leads/lists/{id}/rows/{row_id}", {
        path: { id, row_id: row },
        body: { [col]: value },
      }),
    onSuccess: (row) =>
      qc.setQueryData<RowsReply>(key, (old) =>
        old ? { ...old, rows: old.rows.map((r) => (r.id === row.id ? row : r)) } : old,
      ),
    onError: (e) => toast.error(messageOf(e)),
  });

  const allPicked = data.length > 0 && data.every((r) => picked.has(r.id));
  const from = page * PAGE_SIZE + 1;
  const pager = (
    <nav aria-label="Pages of this list" className="flex items-center gap-2">
      <span className="text-xs text-muted">
        Rows {fmtInt(from)}-{fmtInt(from + data.length - 1)} of {fmtInt(total)}. Page {page + 1} of{" "}
        {fmtInt(pages)}
      </span>
      <Button
        variant="secondary"
        className="h-8"
        disabled={page === 0 || rows.isFetching}
        onClick={() => setCursors((c) => c.slice(0, -1))}
      >
        <ChevronLeft size={14} aria-hidden="true" /> Previous
      </Button>
      <Button
        variant="secondary"
        className="h-8"
        disabled={!nextCursor || rows.isFetching}
        onClick={() => nextCursor && setCursors((c) => [...c, nextCursor])}
      >
        Next <ChevronRight size={14} aria-hidden="true" />
      </Button>
    </nav>
  );
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="ghost" className="h-8" onClick={onBack}>
          <ArrowLeft size={14} aria-hidden="true" /> All lists
        </Button>
        <div className="min-w-0">
          <h2 className="truncate text-lg font-semibold">{list?.name ?? "List"}</h2>
          <p className="text-xs text-muted">
            {fmtInt(list?.row_count ?? 0)} rows, {columns.length} columns. Changes here stay in this
            list.
          </p>
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <input
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="Search this list"
          aria-label="Search this list"
          className="h-9 w-64 rounded-(--radius-control) border border-border bg-(--input-bg) px-3 text-sm"
        />
        <div className="ml-auto flex flex-wrap gap-2">
          {picked.size > 0 && (
            <Button variant="secondary" onClick={() => setDialog("delete")}>
              Delete {picked.size}
            </Button>
          )}
          <Button variant="secondary" onClick={() => setDialog("row")} disabled={!columns.length}>
            Add row
          </Button>
          <Button variant="secondary" onClick={() => setDialog("column")}>
            Add column
          </Button>
          <Button variant="secondary" onClick={() => setDialog("import")}>
            Import CSV
          </Button>
          <Button onClick={() => setDialog("campaign")} disabled={!list?.row_count}>
            Add {picked.size > 0 ? picked.size : "all"} to campaign
          </Button>
        </div>
      </div>
      {rows.isPending && <Skeleton />}
      {rows.isError && <ErrorNote error={rows.error} />}
      {rows.data && data.length === 0 && (
        <EmptyState
          text={
            find
              ? "No row matches."
              : "This list is empty. Import a CSV file. Every column is kept as it is."
          }
        />
      )}
      {data.length > 0 && (
        <>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p className="text-xs text-muted">
              {find ? `${fmtInt(total)} match.` : null} Click a cell to change it.
            </p>
            {pager}
          </div>
          <Table>
            <Head
              cols={[
                <input
                  key="all"
                  type="checkbox"
                  aria-label="Select all rows on this page"
                  checked={allPicked}
                  onChange={(e) =>
                    setPicked(e.target.checked ? new Set(data.map((r) => r.id)) : new Set())
                  }
                />,
                ...columns.map((c) => (
                  <span key={c} className="inline-flex items-center gap-1 whitespace-nowrap">
                    {c}
                    <MenuRoot>
                      <MenuTrigger
                        aria-label={`Column ${c}`}
                        className="rounded p-0.5 hover:bg-surface"
                      >
                        <MoreHorizontal size={13} />
                      </MenuTrigger>
                      <MenuContent align="start">
                        <MenuItem onSelect={() => setDialog({ rename: c })}>Rename</MenuItem>
                        <MenuItem onSelect={() => setDialog({ drop: c })}>Delete column</MenuItem>
                      </MenuContent>
                    </MenuRoot>
                  </span>
                )),
              ]}
            />
            <tbody>
              {data.map((r) => (
                <tr key={r.id} className="border-t border-border">
                  <td className={TD}>
                    <input
                      type="checkbox"
                      aria-label="Select row"
                      checked={picked.has(r.id)}
                      onChange={(e) => {
                        const n = new Set(picked);
                        e.target.checked ? n.add(r.id) : n.delete(r.id);
                        setPicked(n);
                      }}
                    />
                  </td>
                  {columns.map((c) => (
                    <td key={c} className={cn(TD, "p-0")}>
                      <Cell
                        label={c}
                        value={r.data[c] ?? ""}
                        onSave={(value) => saveCell.mutate({ row: r.id, col: c, value })}
                      />
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </Table>
        </>
      )}
      {data.length > 0 && <div className="flex justify-end">{pager}</div>}

      <ImportDialog
        open={dialog === "import"}
        listId={id}
        onClose={() => setDialog(null)}
        onDone={refresh}
      />
      <NameDialog
        open={dialog === "column"}
        title="Add column"
        label="Column name"
        initial=""
        onClose={() => setDialog(null)}
        onSave={async (name) => {
          await http.post("/v1/leads/lists/{id}/columns", { path: { id }, body: { name } });
          refresh();
        }}
      />
      {dialog && typeof dialog === "object" && "rename" in dialog && (
        <NameDialog
          open
          title="Rename column"
          label="New name"
          initial={dialog.rename}
          onClose={() => setDialog(null)}
          onSave={async (to) => {
            await http.patch("/v1/leads/lists/{id}/columns", {
              path: { id },
              body: { from: dialog.rename, to },
            });
            refresh();
          }}
        />
      )}
      <ConfirmDialog
        open={!!dialog && typeof dialog === "object" && "drop" in dialog}
        onClose={() => setDialog(null)}
        title="Delete this column?"
        text="Its values in this list are removed. Leads and other lists do not change."
        confirmLabel="Delete column"
        onConfirm={async () => {
          if (dialog && typeof dialog === "object" && "drop" in dialog) {
            await http.del("/v1/leads/lists/{id}/columns", {
              path: { id },
              body: { name: dialog.drop },
            });
            refresh();
          }
        }}
      />
      <ConfirmDialog
        open={dialog === "delete"}
        onClose={() => setDialog(null)}
        title={`Delete ${picked.size} rows?`}
        text="They are removed from this list only. Leads already made from them stay."
        confirmLabel="Delete rows"
        onConfirm={async () => {
          await http.del("/v1/leads/lists/{id}/rows", {
            path: { id },
            body: { row_ids: [...picked] },
          });
          setPicked(new Set());
          refresh();
        }}
      />
      <RowDialog
        open={dialog === "row"}
        columns={columns}
        onClose={() => setDialog(null)}
        onSave={async (cells) => {
          await http.post("/v1/leads/lists/{id}/rows", { path: { id }, body: { data: cells } });
          refresh();
        }}
      />
      <CampaignDialog
        open={dialog === "campaign"}
        listId={id}
        rowIds={picked.size > 0 ? [...picked] : undefined}
        count={picked.size > 0 ? picked.size : (list?.row_count ?? 0)}
        onClose={() => setDialog(null)}
      />
    </div>
  );
}

/** A cell that turns into a text box on click. Enter or leaving it saves, Escape cancels. */
function Cell({
  label,
  value,
  onSave,
}: {
  label: string;
  value: string;
  onSave: (v: string) => void;
}) {
  const [edit, setEdit] = useState(false);
  const [v, setV] = useState(value);
  if (!edit) {
    return (
      <button
        type="button"
        aria-label={`${label}: ${value || "empty"}. Change`}
        onClick={() => {
          setV(value);
          setEdit(true);
        }}
        className="block h-full min-h-9 w-full max-w-72 truncate px-3 py-2 text-left hover:bg-surface-2"
        title={value}
      >
        {value || <span className="text-muted">-</span>}
      </button>
    );
  }
  const done = (save: boolean) => {
    setEdit(false);
    if (save && v.trim() !== value) onSave(v.trim());
  };
  return (
    <input
      // biome-ignore lint/a11y/noAutofocus: the user clicked the cell to type in it
      autoFocus
      aria-label={label}
      value={v}
      onChange={(e) => setV(e.target.value)}
      onBlur={() => done(true)}
      onKeyDown={(e) => {
        if (e.key === "Enter") done(true);
        if (e.key === "Escape") done(false);
      }}
      className="h-9 w-full min-w-32 border border-accent bg-surface px-3 text-sm"
    />
  );
}

function ImportDialog({
  open,
  listId,
  onClose,
  onDone,
}: {
  open: boolean;
  listId: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const input = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<File | null>(null);
  const [over, setOver] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [res, setRes] = useState<ImportReply | null>(null);
  const close = () => {
    setFile(null);
    setRes(null);
    setError(null);
    onClose();
  };
  async function go() {
    if (!file) return;
    setBusy(true);
    setError(null);
    try {
      const form = new FormData();
      form.append("file", file);
      setRes(await upload<ImportReply>(`/v1/leads/lists/${listId}/import`, form));
      onDone();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      open={open}
      onClose={close}
      size="md"
      title="Import a CSV into this list"
      footer={
        res ? (
          <Button onClick={close}>Done</Button>
        ) : (
          <>
            <Button variant="secondary" onClick={close}>
              Cancel
            </Button>
            <Button onClick={go} disabled={!file} busy={busy}>
              Import
            </Button>
          </>
        )
      }
    >
      {res ? (
        <div className="flex flex-col gap-2 text-sm">
          <Notice tone="success">
            {fmtInt(res.added)} rows added
            {res.empty ? `, ${fmtInt(res.empty)} empty rows left out` : ""}.
          </Notice>
          {res.new_columns.length > 0 && <p>New columns: {res.new_columns.join(", ")}.</p>}
        </div>
      ) : (
        <div className="flex flex-col gap-3">
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
              Every column is kept with its name. Columns with the same name go together.
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
          {error != null && (
            <Notice tone="danger">
              {error instanceof ApiProblem && error.status === 413
                ? "The file is too big."
                : messageOf(error)}
            </Notice>
          )}
        </div>
      )}
    </Modal>
  );
}

function NameDialog({
  open,
  title,
  label,
  initial,
  onClose,
  onSave,
}: {
  open: boolean;
  title: string;
  label: string;
  initial: string;
  onClose: () => void;
  onSave: (name: string) => Promise<unknown>;
}) {
  const [name, setName] = useState(initial);
  const save = useMutation({
    mutationFn: () => onSave(name.trim()),
    onSuccess: () => {
      setName("");
      onClose();
    },
  });
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="sm"
      title={title}
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={save.isPending} disabled={!name.trim()} onClick={() => save.mutate()}>
            Save
          </Button>
        </>
      }
    >
      <Field label={label} value={name} onChange={(e) => setName(e.target.value)} />
      {save.isError && (
        <div className="mt-3">
          <ErrorNote error={save.error} />
        </div>
      )}
    </Modal>
  );
}

function RowDialog({
  open,
  columns,
  onClose,
  onSave,
}: {
  open: boolean;
  columns: string[];
  onClose: () => void;
  onSave: (cells: Record<string, string>) => Promise<unknown>;
}) {
  const [cells, setCells] = useState<Record<string, string>>({});
  const save = useMutation({
    mutationFn: () => onSave(cells),
    onSuccess: () => {
      setCells({});
      onClose();
    },
  });
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="md"
      title="Add row"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            busy={save.isPending}
            disabled={!Object.values(cells).some((v) => v.trim())}
            onClick={() => save.mutate()}
          >
            Add
          </Button>
        </>
      }
    >
      <div className="grid max-h-[60vh] gap-3 overflow-auto sm:grid-cols-2">
        {columns.map((c) => (
          <Field
            key={c}
            label={c}
            value={cells[c] ?? ""}
            onChange={(e) => setCells({ ...cells, [c]: e.target.value })}
          />
        ))}
      </div>
      {save.isError && (
        <div className="mt-3">
          <ErrorNote error={save.error} />
        </div>
      )}
    </Modal>
  );
}

function CampaignDialog({
  open,
  listId,
  rowIds,
  count,
  onClose,
}: {
  open: boolean;
  listId: string;
  rowIds: string[] | undefined;
  count: number;
  onClose: () => void;
}) {
  const campaigns = useQuery({ ...q.campaigns, enabled: open });
  const [cid, setCid] = useState("");
  const close = () => {
    setCid("");
    onClose();
  };
  return (
    <Modal open={open} onClose={close} size="md" title={`Add ${fmtInt(count)} rows to a campaign`}>
      {open && (
        <div className="flex flex-col gap-4">
          <Select
            label="Campaign"
            value={cid}
            onChange={setCid}
            options={[
              { value: "", label: "Choose..." },
              ...(campaigns.data?.campaigns ?? [])
                .filter(
                  (c: S["CampaignSummary"]) => c.status !== "archived" && c.status !== "completed",
                )
                .map((c: S["CampaignSummary"]) => ({ value: c.id, label: c.name })),
            ]}
          />
          <p className="text-xs text-muted">
            Next you match the list columns to lead columns. The list does not change.
          </p>
          {cid && <ListToCampaign key={cid} listId={listId} campaignId={cid} rowIds={rowIds} />}
        </div>
      )}
    </Modal>
  );
}
