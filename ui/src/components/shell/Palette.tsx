import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";
import { saveThemePreference } from "@/api/auth";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { useReoon } from "@/features/reoon/api";
import { cn } from "@/lib/cn";
import { fullName } from "@/lib/format";
import { applyChoice, readChoice, resolve } from "@/theme/theme";
import { Modal, useDebounced } from "../ui";

interface Item {
  id: string;
  group: string;
  label: string;
  hint?: string;
  run: () => void;
}

/** Ctrl/Cmd + K: jump to a page, a setting, a campaign, an account, a list or a lead. */
export function Palette({ open, onClose, ws }: { open: boolean; onClose: () => void; ws: string }) {
  const navigate = useNavigate();
  const [text, setText] = useState("");
  const [index, setIndex] = useState(0);
  const dq = useDebounced(text.trim(), 200);
  const inputRef = useRef<HTMLInputElement>(null);
  const campaigns = useQuery({ ...q.campaigns, enabled: open, staleTime: 60_000 });
  const reoon = useReoon();
  const accounts = useQuery({ ...q.accounts, enabled: open, staleTime: 60_000 });
  const lists = useQuery({ ...q.lists, enabled: open, staleTime: 60_000 });
  const leads = useQuery({
    queryKey: ["palette", "leads", dq],
    enabled: open && dq.length >= 2,
    queryFn: () => http.get<S["ListLeadsReply"]>("/v1/leads", { query: { q: dq, limit: 5 } }),
  });
  useEffect(() => {
    if (open) {
      setText("");
      setIndex(0);
      setTimeout(() => inputRef.current?.focus(), 0);
    }
  }, [open]);

  const go =
    (to: string, params: Record<string, string> = { ws }, search?: Record<string, string>) =>
    () => {
      onClose();
      void navigate({ to, params, ...(search ? { search } : {}) } as never);
    };

  const items: Item[] = useMemo(() => {
    const p = (
      id: string,
      group: string,
      label: string,
      to: string,
      extra?: Partial<Item>,
      params?: Record<string, string>,
      search?: Record<string, string>,
    ): Item => ({ id, group, label, run: go(to, params, search), ...extra });
    const first = accounts.data?.[0]?.id;
    const pages: Item[] = [
      p("p-ai", "Pages and settings", "Aikido AI", "/w/$ws/ai"),
      ...(reoon.installed
        ? [p("p-reoon", "Pages and settings", "Reoon", "/w/$ws/apps/reoon")]
        : []),
      p("p-camp", "Pages and settings", "Campaigns", "/w/$ws/campaigns"),
      p("p-auto", "Pages and settings", "Automations", "/w/$ws/automations"),
      p("p-leads", "Pages and settings", "Leads", "/w/$ws/leads"),
      p("p-lists", "Pages and settings", "Leads: Lists", "/w/$ws/leads", {}, undefined, {
        tab: "lists",
      }),
      p("p-fields", "Pages and settings", "Leads: Custom fields", "/w/$ws/leads", {}, undefined, {
        tab: "fields",
      }),
      p("p-imports", "Pages and settings", "Leads: Imports", "/w/$ws/leads", {}, undefined, {
        tab: "imports",
      }),
      p("p-block", "Pages and settings", "Leads: Blocklist", "/w/$ws/leads", {}, undefined, {
        tab: "blocklist",
      }),
      p("p-inbox", "Pages and settings", "Inbox", "/w/$ws/inbox"),
      p("p-acc", "Pages and settings", "Email Accounts", "/w/$ws/accounts"),
      p("p-pers", "Pages and settings", "Personalisation", "/w/$ws/personalization"),
      p("p-rep", "Pages and settings", "Reports", "/w/$ws/reports"),
      p("p-set", "Pages and settings", "Settings: Account", "/w/$ws/settings/account"),
      p("p-api", "Pages and settings", "Settings: API keys", "/w/$ws/settings/api"),
      p("p-not", "Pages and settings", "Settings: Notifications", "/w/$ws/settings/notifications"),
      p("p-int", "Pages and settings", "Settings: Integrations", "/w/$ws/settings/integrations"),
      p(
        "p-aim",
        "Pages and settings",
        "Settings: AI models",
        "/w/$ws/settings/integrations/$id",
        { hint: "Integrations" },
        { ws, id: "ai" },
      ),
      p("p-team", "Pages and settings", "Settings: Team", "/w/$ws/settings/team"),
      p("p-send", "Pages and settings", "Settings: Sending defaults", "/w/$ws/settings/sending"),
      p("p-stat", "Pages and settings", "Status", "/w/$ws/status"),
      ...(first
        ? [
            p(
              "p-daily",
              "Pages and settings",
              "Daily limit",
              "/w/$ws/accounts/$id/sending",
              { hint: "Email Accounts" },
              { ws, id: first },
            ),
            p(
              "p-rc",
              "Pages and settings",
              "Reply check settings",
              "/w/$ws/accounts/$id/reply-check",
              { hint: "Email Accounts" },
              { ws, id: first },
            ),
          ]
        : []),
    ];
    const actions: Item[] = [
      { id: "a-camp", group: "Actions", label: "New campaign", run: go("/w/$ws/campaigns") },
      { id: "a-acc", group: "Actions", label: "Connect email account", run: go("/w/$ws/accounts") },
      {
        id: "a-imp",
        group: "Actions",
        label: "Import leads",
        run: go("/w/$ws/leads", { ws }, { tab: "imports" }),
      },
      {
        id: "a-theme",
        group: "Actions",
        label: "Toggle theme",
        run: () => {
          onClose();
          const next = resolve(readChoice()) === "dark" ? "light" : "dark";
          applyChoice(next);
          void saveThemePreference(next).catch(() => {});
        },
      },
    ];
    const data: Item[] = [
      ...(campaigns.data?.campaigns ?? []).map((c) =>
        p(`c-${c.id}`, "Campaigns", c.name, "/w/$ws/campaigns/$id", {}, { ws, id: c.id }),
      ),
      ...(accounts.data ?? []).map((a) =>
        p(`a-${a.id}`, "Email accounts", a.email, "/w/$ws/accounts/$id", {}, { ws, id: a.id }),
      ),
      ...(lists.data ?? []).map((l) =>
        p(`l-${l.id}`, "Lists", l.name, "/w/$ws/leads", {}, undefined, { list_id: l.id }),
      ),
      ...(leads.data?.leads ?? []).map((l) =>
        p(
          `ld-${l.id}`,
          "Leads",
          fullName(l) || l.email,
          "/w/$ws/leads",
          { hint: l.email },
          undefined,
          { lead: l.id },
        ),
      ),
    ];
    return [...actions, ...pages, ...data];
  }, [ws, campaigns.data, accounts.data, lists.data, leads.data, reoon.installed]);

  const shown = useMemo(() => {
    const t = text.trim().toLowerCase();
    if (!t)
      return items
        .filter((i) => i.group === "Actions" || i.group === "Pages and settings")
        .slice(0, 12);
    return items
      .filter(
        (i) =>
          i.group === "Leads" || `${i.label} ${i.hint ?? ""} ${i.group}`.toLowerCase().includes(t),
      )
      .slice(0, 30);
  }, [items, text]);
  useEffect(() => setIndex(0), [text]);

  function onKey(e: React.KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setIndex((i) => Math.min(i + 1, shown.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      shown[index]?.run();
    }
  }
  let lastGroup = "";
  return (
    <Modal open={open} onClose={onClose} size="md" title="Search">
      <div className="flex flex-col gap-2">
        <input
          ref={inputRef}
          onKeyDown={onKey}
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-list"
          aria-activedescendant={shown[index] ? `pal-${shown[index].id}` : undefined}
          aria-label="Search pages, settings, campaigns, accounts, lists and leads"
          placeholder="Search..."
          value={text}
          onChange={(e) => setText(e.target.value)}
          className="h-10 w-full rounded-(--radius-control) border border-border bg-surface-2 px-3"
        />
        <div id="palette-list" role="listbox" className="max-h-80 overflow-auto">
          {shown.length === 0 && <p className="px-2 py-3 text-muted">Nothing found.</p>}
          {shown.map((it, i) => {
            const header = it.group !== lastGroup ? it.group : null;
            lastGroup = it.group;
            return (
              <div key={it.id} role="presentation">
                {header && (
                  <p className="px-2 pb-1 pt-2 text-xs font-medium text-muted">{header}</p>
                )}
                <button
                  id={`pal-${it.id}`}
                  role="option"
                  aria-selected={i === index}
                  type="button"
                  onClick={it.run}
                  onMouseEnter={() => setIndex(i)}
                  className={cn(
                    "flex w-full items-center justify-between rounded-(--radius-control) px-2 py-1.5 text-left text-sm",
                    i === index && "bg-accent-soft",
                  )}
                >
                  <span>{it.label}</span>
                  {it.hint && <span className="text-xs text-muted">{it.hint}</span>}
                </button>
              </div>
            );
          })}
        </div>
      </div>
    </Modal>
  );
}
