import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { ChevronDown, MessageSquarePlus, Send, Sparkles, Trash2, Undo2, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import {
  MenuContent,
  MenuItem,
  MenuRoot,
  MenuSeparator,
  MenuTrigger,
} from "@/components/shell/menu";
import { Button } from "@/components/ui";
import { cn } from "@/lib/cn";
import { copilot } from "./api";
import {
  type AppliedOk,
  type ChatMessage,
  type Chip,
  type CopilotIssue,
  chipOf,
  errorShown,
  fixText,
  flashIds,
  isOk,
  MAX_MESSAGE,
  mergeMessages,
  type Shown,
  sendCheck,
  type Thread,
  takeWantChat,
  threadTitle,
  undoWords,
  wantChat,
} from "./copilot";
import type { WorkflowDetail } from "./types";

export { takeWantChat };

const tkey = (w: string | undefined) => ["automations", "copilot", "threads", w ?? "none"] as const;
const mkey = (t: string) => ["automations", "copilot", "messages", t] as const;

/** The small box in the editor header. */
export function AskAiBox({ onClick, active }: { onClick: () => void; active: boolean }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={cn(
        "flex h-8 w-64 max-w-full items-center gap-2 rounded-(--radius-control) border border-border bg-surface px-2.5 text-left text-xs text-muted hover:bg-surface-2",
        active && "border-accent",
      )}
    >
      <Sparkles size={14} aria-hidden="true" className="shrink-0 text-ai" />
      <span className="truncate">Ask AI to build or change this workflow</span>
    </button>
  );
}

function ErrorLine({ shown, ws }: { shown: Shown; ws?: string }) {
  return (
    <div
      role="alert"
      className="rounded-(--radius-control) border border-danger/40 bg-danger/10 px-3 py-2 text-sm"
    >
      {shown.text}
      {shown.link === "ai-settings" && ws && (
        <>
          {" "}
          <Link to="/w/$ws/settings/integrations/$id" params={{ ws, id: "ai" }} className="underline">
            Open the AI settings
          </Link>
        </>
      )}
    </div>
  );
}

/** The textarea: Enter sends, Shift+Enter is a new line. */
function Composer({
  value,
  onChange,
  onSend,
  busy,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  onSend: () => void;
  busy: boolean;
  placeholder: string;
}) {
  const c = sendCheck(value);
  const near = value.length > MAX_MESSAGE * 0.8;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-end gap-2">
        <textarea
          aria-label="Message to the AI"
          value={value}
          rows={2}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              if (c.ok && !busy) onSend();
            }
          }}
          className="max-h-40 min-h-12 flex-1 resize-y rounded-(--radius-control) border border-border bg-surface px-2.5 py-2 text-sm"
        />
        <Button aria-label="Send" busy={busy} disabled={!c.ok} onClick={onSend} className="h-9">
          <Send size={14} aria-hidden="true" />
        </Button>
      </div>
      {near && (
        <p className={cn("text-right text-xs", c.over ? "text-danger" : "text-muted")}>
          {value.length} / {MAX_MESSAGE}
          {c.over ? ` (${c.over} too many)` : ""}
        </p>
      )}
    </div>
  );
}

/** The list page and the New page: a prompt makes a new workflow, then the editor opens with the chat. */
export function AskAiCreate({ ws }: { ws: string }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [thread, setThread] = useState<string | null>(null);
  const [shown, setShown] = useState<Shown | null>(null);
  const [failed, setFailed] = useState<{ content: string; chip: Chip } | null>(null);

  async function go() {
    if (busy || !sendCheck(text).ok) return;
    setBusy(true);
    setShown(null);
    setFailed(null);
    try {
      const t = thread ?? (await copilot.createThread()).id;
      setThread(t);
      const r = await copilot.send(t, { text });
      void client.invalidateQueries({ queryKey: ["automations", "list"] });
      if (r.workflow_id) {
        wantChat(r.workflow_id);
        await navigate({ to: "/w/$ws/automations/$id", params: { ws, id: r.workflow_id } });
        return;
      }
      const chip = chipOf(r.message.applied);
      setFailed({ content: r.message.content, chip: chip ?? { kind: "proposed", text: "" } });
      if (!chip) setText("");
    } catch (e) {
      setShown(errorShown(e));
    }
    setBusy(false);
  }

  return (
    <div className="flex flex-col gap-2">
      <Composer
        value={text}
        onChange={setText}
        onSend={() => void go()}
        busy={busy}
        placeholder="Describe the workflow you want"
      />
      {busy && <p className="text-xs text-muted">The AI is building it. This can take a minute.</p>}
      {shown && <ErrorLine shown={shown} ws={ws} />}
      {failed && (
        <div className="rounded-(--radius-control) border border-border bg-surface-2 px-3 py-2 text-sm">
          {failed.content && <p className="whitespace-pre-wrap">{failed.content}</p>}
          {failed.chip.kind === "failed" && (
            <ErrorList text={failed.chip.text} errors={failed.chip.errors} />
          )}
        </div>
      )}
    </div>
  );
}

function ErrorList({ text, errors }: { text: string; errors: CopilotIssue[] }) {
  return (
    <div className="mt-2">
      <p className="font-medium text-danger">{text}</p>
      <ul className="mt-1 list-disc space-y-0.5 pl-5 text-xs">
        {errors.map((e) => (
          <li key={`${e.code}${e.node_id ?? ""}${e.path ?? ""}${e.message}`}>
            {e.node_id && <code>{e.node_id}: </code>}
            {e.message}
          </li>
        ))}
      </ul>
    </div>
  );
}

interface PanelProps {
  ws: string;
  workflowId: string;
  /** Save what waits in the editor; false: it could not be saved. */
  flush: () => Promise<boolean>;
  /** The AI changed the draft on the server: load it and flash these nodes. */
  onChanged: (flash: string[]) => void;
  /** Undo gave the old draft back. */
  onUndone: (w: WorkflowDetail) => void;
  /** The AI is working: the editor is locked meanwhile. */
  onBusy: (busy: boolean) => void;
  onSelectNode: (id: string) => void;
  onClose: () => void;
}

/** The chat panel of an open workflow (map 16.1). */
export function CopilotPanel(p: PanelProps) {
  const client = useQueryClient();
  const threads = useQuery({
    queryKey: tkey(p.workflowId),
    queryFn: async () => (await copilot.threads(p.workflowId)).threads,
  });
  const [pick, setPick] = useState<string | "new" | null>(null);
  const list = threads.data ?? [];
  const active: Thread | undefined =
    pick === "new" ? undefined : (list.find((t) => t.id === pick) ?? list[0]);
  const msgs = useQuery({
    queryKey: mkey(active?.id ?? "none"),
    enabled: !!active,
    queryFn: async () => (await copilot.messages(active?.id ?? "")).messages,
  });
  const [text, setText] = useState("");
  const [pending, setPending] = useState<string | null>(null);
  const [shown, setShown] = useState<Shown | null>(null);
  const [changes, setChanges] = useState<AppliedOk | null>(null);
  const [undoing, setUndoing] = useState<number | null>(null);
  const bottom = useRef<HTMLDivElement>(null);
  const messages = msgs.data ?? [];

  useEffect(() => {
    bottom.current?.scrollIntoView?.({ block: "end" });
  }, [messages.length, pending]);

  async function send(body: string) {
    if (pending !== null || !sendCheck(body).ok) return;
    setShown(null);
    setPending(body);
    p.onBusy(true);
    try {
      if (!(await p.flush())) {
        setShown({
          text: "Your latest edits could not be saved first, so the AI would not see them. Try again.",
        });
        return;
      }
      let t = active?.id;
      if (!t) {
        t = (await copilot.createThread(p.workflowId)).id;
        setPick(t);
      }
      const r = await copilot.send(t, { text: body, workflow_id: p.workflowId });
      client.setQueryData<ChatMessage[]>(mkey(t), (old) =>
        mergeMessages(old ?? [], [r.user_message, r.message]),
      );
      void client.invalidateQueries({ queryKey: tkey(p.workflowId) });
      setText("");
      const ap = r.message.applied;
      if (isOk(ap) && ap.mode === "replace") p.onChanged(flashIds(ap));
    } catch (e) {
      setShown(errorShown(e));
    } finally {
      setPending(null);
      p.onBusy(false);
    }
  }

  async function undo(m: ChatMessage) {
    setUndoing(m.id);
    try {
      const r = await copilot.undo(m.id);
      const t = active?.id;
      if (t)
        client.setQueryData<ChatMessage[]>(mkey(t), (old) => mergeMessages(old ?? [], [r.message]));
      p.onUndone(r.workflow);
    } catch (e) {
      toast.error(undoWords(e));
    } finally {
      setUndoing(null);
    }
  }

  async function remove() {
    if (!active) return;
    try {
      await copilot.deleteThread(active.id);
      setPick(null);
      void client.invalidateQueries({ queryKey: tkey(p.workflowId) });
    } catch (e) {
      toast.error(errorShown(e).text);
    }
  }

  return (
    <aside
      aria-label="AI chat"
      className="fixed inset-0 z-40 flex flex-col bg-surface md:static md:z-auto md:w-[380px] md:shrink-0 md:border-l md:border-border"
    >
      <header className="flex items-center gap-1 border-b border-border px-3 py-2">
        <Sparkles size={15} aria-hidden="true" className="shrink-0 text-ai" />
        <h2
          className="min-w-0 flex-1 truncate text-sm font-semibold"
          title={active ? threadTitle(active) : "New chat"}
        >
          {active ? threadTitle(active) : "New chat"}
        </h2>
        <button
          type="button"
          aria-label="New chat"
          title="New chat"
          onClick={() => {
            setPick("new");
            setShown(null);
            setChanges(null);
          }}
          className="flex size-8 items-center justify-center rounded-(--radius-control) hover:bg-surface-2"
        >
          <MessageSquarePlus size={15} />
        </button>
        <MenuRoot>
          <MenuTrigger
            aria-label="Chats"
            className="flex size-8 items-center justify-center rounded-(--radius-control) hover:bg-surface-2"
          >
            <ChevronDown size={15} />
          </MenuTrigger>
          <MenuContent align="end">
            {list.map((t) => (
              <MenuItem key={t.id} onSelect={() => setPick(t.id)}>
                <span className={cn("max-w-56 truncate", t.id === active?.id && "font-semibold")}>
                  {threadTitle(t)}
                </span>
              </MenuItem>
            ))}
            {list.length === 0 && <p className="px-3 py-1.5 text-xs text-muted">No older chats.</p>}
            <MenuSeparator />
            <MenuItem onSelect={() => void remove()}>
              <Trash2 size={15} aria-hidden="true" /> Delete this chat
            </MenuItem>
          </MenuContent>
        </MenuRoot>
        <button
          type="button"
          aria-label="Close the chat"
          onClick={p.onClose}
          className="flex size-8 items-center justify-center rounded-(--radius-control) hover:bg-surface-2"
        >
          <X size={15} />
        </button>
      </header>

      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-3">
        {messages.length === 0 && pending === null && (
          <p className="text-sm text-muted">
            Tell the AI what this workflow should do, or what to change. It sees your current draft.
          </p>
        )}
        {messages.map((m) => (
          <Bubble
            key={m.id}
            m={m}
            busy={pending !== null}
            undoing={undoing === m.id}
            onUndo={() => void undo(m)}
            onView={(a) => setChanges(a)}
            onFix={(errs) => void send(fixText(errs))}
          />
        ))}
        {pending !== null && (
          <>
            <div className="ml-8 whitespace-pre-wrap rounded-lg bg-accent-soft px-3 py-2 text-sm">
              {pending}
            </div>
            <p role="status" className="animate-pulse text-sm text-muted">
              The AI is thinking. This can take a minute.
            </p>
          </>
        )}
        {shown && <ErrorLine shown={shown} ws={p.ws} />}
        <div ref={bottom} />
      </div>

      {changes && (
        <ChangesPanel a={changes} onSelect={p.onSelectNode} onClose={() => setChanges(null)} />
      )}

      <div className="border-t border-border p-3">
        <Composer
          value={text}
          onChange={setText}
          onSend={() => void send(text)}
          busy={pending !== null}
          placeholder="Ask AI to build or change this workflow"
        />
      </div>
    </aside>
  );
}

function Bubble({
  m,
  busy,
  undoing,
  onUndo,
  onView,
  onFix,
}: {
  m: ChatMessage;
  busy: boolean;
  undoing: boolean;
  onUndo: () => void;
  onView: (a: AppliedOk) => void;
  onFix: (e: CopilotIssue[]) => void;
}) {
  if (m.role === "user")
    return (
      <div className="ml-8 whitespace-pre-wrap rounded-lg bg-accent-soft px-3 py-2 text-sm">
        {m.content}
      </div>
    );
  const chip = chipOf(m.applied);
  return (
    <div className="mr-4 rounded-lg bg-surface-2 px-3 py-2 text-sm">
      {m.content && <p className="whitespace-pre-wrap">{m.content}</p>}
      {chip && (
        <div className="mt-2 border-t border-border pt-2">
          {chip.kind === "failed" ? (
            <>
              <ErrorList text={chip.text} errors={chip.errors} />
              {chip.errors.length > 0 && (
                <Button
                  variant="secondary"
                  className="mt-2 h-7"
                  disabled={busy}
                  onClick={() => onFix(chip.errors)}
                >
                  Try to fix
                </Button>
              )}
            </>
          ) : (
            <>
              <p className="text-xs font-medium">{chip.text}</p>
              {isOk(m.applied) && (
                <div className="mt-1.5 flex gap-2">
                  {chip.kind === "applied" && chip.canUndo && (
                    <Button variant="secondary" className="h-7" busy={undoing} onClick={onUndo}>
                      <Undo2 size={13} aria-hidden="true" /> Undo
                    </Button>
                  )}
                  <Button
                    variant="secondary"
                    className="h-7"
                    onClick={() => onView(m.applied as AppliedOk)}
                  >
                    View changes
                  </Button>
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

function ChangesPanel({
  a,
  onSelect,
  onClose,
}: {
  a: AppliedOk;
  onSelect: (id: string) => void;
  onClose: () => void;
}) {
  const group = (label: string, ids: string[], click: boolean) =>
    ids.length > 0 && (
      <div>
        <p className="text-xs font-medium text-muted">{label}</p>
        <div className="mt-1 flex flex-wrap gap-1">
          {ids.map((id) =>
            click ? (
              <button
                key={id}
                type="button"
                onClick={() => onSelect(id)}
                className="rounded border border-border px-1.5 py-0.5 text-xs hover:bg-surface-2"
              >
                {id}
              </button>
            ) : (
              <span
                key={id}
                className="rounded border border-border px-1.5 py-0.5 text-xs text-muted line-through"
              >
                {id}
              </span>
            ),
          )}
        </div>
      </div>
    );
  return (
    <section
      aria-label="Changes"
      className="max-h-48 space-y-2 overflow-y-auto border-t border-border bg-surface-2 p-3"
    >
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold">Changes</h3>
        <button
          type="button"
          aria-label="Close the changes"
          onClick={onClose}
          className="text-muted hover:text-text"
        >
          <X size={14} />
        </button>
      </div>
      {group("Added", a.diff.added, true)}
      {group("Changed", a.diff.changed, true)}
      {group("Removed", a.diff.removed, false)}
      <p className="text-xs text-muted">
        Connections: {a.diff.edges_added} added, {a.diff.edges_removed} removed.
      </p>
    </section>
  );
}
