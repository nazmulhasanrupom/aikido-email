import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Bell, Menu, Search } from "lucide-react";
import { q } from "@/api/queries";
import { cn } from "@/lib/cn";
import { fmtAgo } from "@/lib/format";
import { MenuContent, MenuItem, MenuLabel, MenuRoot, MenuTrigger } from "./menu";

const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.platform);

/** The top bar: the search box (opens the palette) and the bell (alerts, checked every minute). */
export function TopBar({
  onOpenDrawer,
  onOpenSearch,
}: {
  onOpenDrawer: () => void;
  onOpenSearch: () => void;
}) {
  const alerts = useQuery(q.alerts);
  const list = alerts.data ?? [];
  const errors = list.filter((a) => a.severity === "error").length;
  return (
    <header className="ui-topbar flex h-12 shrink-0 items-center gap-3 border-b border-border bg-(--topbar-bg) px-4">
      <button
        type="button"
        onClick={onOpenDrawer}
        aria-label="Open menu"
        className="rounded-(--radius-control) p-1.5 hover:bg-surface-2 md:hidden"
      >
        <Menu size={20} />
      </button>
      <div className="flex flex-1 justify-center">
        <button
          type="button"
          onClick={onOpenSearch}
          aria-label="Search"
          className="flex h-8 w-full max-w-md items-center gap-2 rounded-(--radius-button) border border-border bg-surface px-3 text-sm text-muted hover:bg-surface-2"
        >
          <Search size={14} aria-hidden="true" />
          <span className="flex-1 text-left">Search</span>
          <kbd className="rounded border border-border px-1.5 font-mono text-xs">
            {isMac ? "⌘ K" : "Ctrl K"}
          </kbd>
        </button>
      </div>
      <MenuRoot>
        <MenuTrigger
          aria-label={list.length ? `Alerts, ${list.length} open` : "Alerts, none"}
          className="relative rounded-(--radius-control) p-1.5 hover:bg-surface-2"
        >
          <Bell size={18} />
          {list.length > 0 && (
            <span
              className={cn(
                "absolute -right-0.5 -top-0.5 flex min-w-4 items-center justify-center rounded-full px-1 text-[10px] font-semibold text-accent-fg",
                errors ? "bg-danger" : "bg-accent",
              )}
            >
              {list.length}
            </span>
          )}
        </MenuTrigger>
        <MenuContent align="end" className="w-96 max-w-[92vw]">
          <MenuLabel>Alerts</MenuLabel>
          {list.length === 0 && <p className="px-2 py-3 text-sm text-muted">All clear.</p>}
          {list.map((a) => (
            <MenuItem key={a.id} asChild>
              <Link to={a.link as never} className="flex flex-col items-start gap-0.5">
                <span className="flex items-center gap-2 font-medium">
                  <span
                    aria-hidden="true"
                    className={cn(
                      "size-2 rounded-full",
                      a.severity === "error"
                        ? "bg-danger"
                        : a.severity === "warning"
                          ? "bg-warning"
                          : "bg-info",
                    )}
                  />
                  {a.title}
                </span>
                <span className="text-xs text-muted">{a.detail}</span>
                <span className="text-xs text-muted">{fmtAgo(a.since)}</span>
              </Link>
            </MenuItem>
          ))}
        </MenuContent>
      </MenuRoot>
    </header>
  );
}
