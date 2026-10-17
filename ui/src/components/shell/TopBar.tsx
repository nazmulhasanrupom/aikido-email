import { Bell, Menu, Search } from "lucide-react";

/**
 * The top bar. The search box and the bell are in place; they work in a later phase
 * (the command palette and the alerts list).
 */
export function TopBar({ onOpenDrawer }: { onOpenDrawer: () => void }) {
  return (
    <header className="flex h-12 shrink-0 items-center gap-3 border-b border-border bg-bg px-4">
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
          disabled
          aria-label="Search (coming soon)"
          title="Search comes soon"
          className="flex h-8 w-full max-w-md items-center gap-2 rounded-(--radius-control) border border-border bg-surface px-3 text-sm text-muted"
        >
          <Search size={14} aria-hidden="true" />
          <span className="flex-1 text-left">Search</span>
          <kbd className="rounded border border-border px-1.5 font-mono text-xs">Ctrl K</kbd>
        </button>
      </div>
      <button
        type="button"
        disabled
        aria-label="Alerts (coming soon)"
        title="Alerts come soon"
        className="rounded-(--radius-control) p-1.5 text-muted"
      >
        <Bell size={18} />
      </button>
    </header>
  );
}
