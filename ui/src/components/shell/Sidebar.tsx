import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
  ChartColumn,
  ChevronsUpDown,
  Inbox,
  LogOut,
  Mail,
  Monitor,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Send,
  Settings,
  ShieldCheck,
  Sparkles,
  Sun,
  Users,
} from "lucide-react";
import { type ReactNode, useState } from "react";
import { toast } from "sonner";
import { type AuthState, authStateKey, type User } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { useReoon } from "@/features/reoon/api";
import { useUser } from "@/features/settings/useUser";
import { cn } from "@/lib/cn";
import { type ThemeChoice, useThemeChoice } from "@/theme/theme";
import { Button } from "../Button";
import { Logo } from "../Logo";
import { Field, Modal, Notice } from "../ui";
import {
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuRadioGroup,
  MenuRadioItem,
  MenuRoot,
  MenuSeparator,
  MenuTrigger,
} from "./menu";

interface NavItem {
  to:
    | "/w/$ws/ai"
    | "/w/$ws/campaigns"
    | "/w/$ws/leads"
    | "/w/$ws/inbox"
    | "/w/$ws/accounts"
    | "/w/$ws/reports";
  label: string;
  icon: typeof Send;
  soon?: boolean;
}

const NAV: NavItem[] = [
  { to: "/w/$ws/ai", label: "Aikido AI", icon: Sparkles, soon: true },
  { to: "/w/$ws/campaigns", label: "Campaigns", icon: Send },
  { to: "/w/$ws/leads", label: "Leads", icon: Users },
  { to: "/w/$ws/inbox", label: "Inbox", icon: Inbox },
  { to: "/w/$ws/accounts", label: "Email Accounts", icon: Mail },
  { to: "/w/$ws/reports", label: "Reports", icon: ChartColumn },
];

function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  return (
    (parts[0]?.[0] ?? "?") + (parts.length > 1 ? (parts[parts.length - 1]?.[0] ?? "") : "")
  ).toUpperCase();
}

interface Props {
  ws: string;
  state: AuthState;
  user: User;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** The phone drawer is open. */
  drawerOpen: boolean;
  onCloseDrawer: () => void;
  onChangeTheme: (choice: ThemeChoice) => void;
  onLogout: () => void;
}

export function Sidebar(p: Props) {
  // The icon-only mode is for wide screens. In the phone drawer the labels always show.
  const slim = p.collapsed;
  const seen = (p.user.preferences as { last_seen_inbox_at?: string } | undefined)
    ?.last_seen_inbox_at;
  const unread = useQuery({
    queryKey: ["inbox", "badge", seen],
    queryFn: () =>
      http.get<S["ListMessagesReply"]>("/v1/replies/messages", {
        query: { label: "reply", since: seen, limit: 100 },
      }),
    refetchInterval: 60_000,
  });
  const inboxCount = unread.data?.messages.length ?? 0;
  const reoon = useReoon();
  return (
    <>
      {p.drawerOpen && (
        <button
          type="button"
          aria-label="Close menu"
          onClick={p.onCloseDrawer}
          className="animate-fade-in fixed inset-0 z-30 bg-black/40 md:hidden"
        />
      )}
      <aside
        aria-label="Main menu"
        className={cn(
          "fixed inset-y-0 left-0 z-40 flex w-60 flex-col border-r border-border bg-surface transition-transform duration-150 md:static md:translate-x-0",
          p.drawerOpen ? "translate-x-0" : "-translate-x-full",
          slim ? "md:w-14" : "md:w-58",
        )}
      >
        <div
          className={cn("flex items-center p-2", slim ? "md:justify-center" : "justify-between")}
        >
          <WorkspaceMenu name={p.state.workspace_name} logo={p.state.workspace_logo} slim={slim} />
        </div>

        <nav aria-label="Pages" className="flex flex-1 flex-col gap-0.5 px-2 py-2">
          {NAV.map((item) => (
            <Link
              key={item.to}
              to={item.to}
              params={{ ws: p.ws }}
              onClick={p.onCloseDrawer}
              title={slim ? item.label : undefined}
              className={cn(
                "flex h-9 items-center gap-3 rounded-(--radius-control) px-2.5 text-sm hover:bg-surface-2",
                slim && "md:justify-center md:px-0",
              )}
              activeProps={{ className: "bg-accent-soft font-medium hover:bg-accent-soft" }}
            >
              <item.icon size={18} aria-hidden="true" className="shrink-0" />
              <span className={cn("flex-1 truncate", slim && "md:hidden")}>{item.label}</span>
              {item.soon && (
                <span
                  className={cn(
                    "rounded bg-surface-2 px-1.5 py-0.5 text-xs text-muted",
                    slim && "md:hidden",
                  )}
                >
                  Soon
                </span>
              )}
              {item.to === "/w/$ws/inbox" && inboxCount > 0 ? (
                <span
                  className="rounded-full bg-accent px-1.5 text-xs text-accent-fg"
                  aria-label={`${inboxCount} new replies`}
                >
                  {inboxCount > 99 ? "99+" : inboxCount}
                </span>
              ) : null}
            </Link>
          ))}
          {reoon.installed && (
            <>
              <div className={cn("mt-3 px-2.5 text-xs font-medium text-muted", slim && "md:hidden")}>Apps</div>
              <Link
                to="/w/$ws/apps/reoon"
                params={{ ws: p.ws }}
                onClick={p.onCloseDrawer}
                title={slim ? "Reoon" : undefined}
                className={cn(
                  "flex h-9 items-center gap-3 rounded-(--radius-control) px-2.5 text-sm hover:bg-surface-2",
                  slim && "md:justify-center md:px-0",
                )}
                activeProps={{ className: "bg-accent-soft font-medium hover:bg-accent-soft" }}
              >
                <ShieldCheck size={18} aria-hidden="true" className="shrink-0" />
                <span className={cn("flex-1 truncate", slim && "md:hidden")}>Reoon</span>
              </Link>
            </>
          )}
        </nav>

        <div className="border-t border-border p-2">
          <button
            type="button"
            onClick={p.onToggleCollapsed}
            aria-label={slim ? "Expand the menu" : "Collapse the menu"}
            className="mb-1 hidden h-8 w-full items-center gap-3 rounded-(--radius-control) px-2.5 text-muted hover:bg-surface-2 md:flex"
          >
            {slim ? <PanelLeftOpen size={18} className="mx-auto" /> : <PanelLeftClose size={18} />}
            {!slim && <span className="text-sm">Collapse</span>}
          </button>
          <UserMenu {...p} slim={slim} />
        </div>
      </aside>
    </>
  );
}

function WorkspaceMenu({
  name,
  logo,
  slim,
}: {
  name: string;
  logo?: string | null | undefined;
  slim: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const { isAdmin } = useUser();
  return (
    <>
      <WorkspaceSettings open={editing} onClose={() => setEditing(false)} name={name} logo={logo} />
      <MenuRoot>
        <MenuTrigger
          aria-label={`Workspace ${name}`}
          className={cn(
            "flex h-10 min-w-0 items-center gap-2.5 rounded-(--radius-control) px-1.5 hover:bg-surface-2",
            slim ? "md:w-10 md:justify-center" : "flex-1",
          )}
        >
          {logo ? (
            <img src={logo} alt="" className="size-7 shrink-0 rounded-md object-cover" />
          ) : (
            <Logo size={28} />
          )}
          <span className={cn("flex-1 truncate text-left font-semibold", slim && "md:hidden")}>
            {name}
          </span>
          <ChevronsUpDown
            size={14}
            className={cn("shrink-0 text-muted", slim && "md:hidden")}
            aria-hidden="true"
          />
        </MenuTrigger>
        <MenuContent align="start">
          <MenuLabel>Workspace</MenuLabel>
          <MenuItem disabled>
            {name}
            <span className="ml-auto text-xs text-muted">Current</span>
          </MenuItem>
          {isAdmin && <MenuItem onSelect={() => setEditing(true)}>Workspace settings</MenuItem>}
          <MenuSeparator />
          <MenuItem disabled>
            New workspace
            <span className="ml-auto rounded bg-surface-2 px-1.5 py-0.5 text-xs text-muted">
              Later
            </span>
          </MenuItem>
        </MenuContent>
      </MenuRoot>
    </>
  );
}

function WorkspaceSettings({
  open,
  onClose,
  name,
  logo,
}: {
  open: boolean;
  onClose: () => void;
  name: string;
  logo?: string | null | undefined;
}) {
  const qc = useQueryClient();
  const [n, setN] = useState(name);
  const [l, setL] = useState<string | null>(logo ?? null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  function pick(f: File | undefined) {
    if (!f) return;
    if (f.size > 150_000) {
      setError("The logo must be smaller than 150 KB.");
      return;
    }
    const r = new FileReader();
    r.onload = () => {
      setL(String(r.result));
      setError(null);
    };
    r.readAsDataURL(f);
  }
  async function save() {
    setBusy(true);
    try {
      await http.patch("/v1/ui/workspace", { body: { name: n, logo: l } });
      await qc.invalidateQueries({ queryKey: authStateKey });
      toast.success("Saved.");
      onClose();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="sm"
      title="Workspace settings"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={busy} disabled={!n.trim()} onClick={save}>
            Save
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <Field label="Name" value={n} onChange={(e) => setN(e.target.value)} />
        <div className="flex items-center gap-3">
          {l ? (
            <img src={l} alt="" className="size-10 rounded-md object-cover" />
          ) : (
            <Logo size={40} />
          )}
          <label className="text-sm">
            <span className="mb-1 block font-medium">Logo (PNG or JPG, under 150 KB)</span>
            <input
              type="file"
              accept="image/png,image/jpeg"
              onChange={(e) => pick(e.target.files?.[0])}
            />
          </label>
          {l && (
            <Button variant="ghost" onClick={() => setL(null)}>
              Remove
            </Button>
          )}
        </div>
        {error && <Notice tone="danger">{error}</Notice>}
      </div>
    </Modal>
  );
}

const THEMES: { value: ThemeChoice; label: string; icon: typeof Sun }[] = [
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
  { value: "system", label: "System", icon: Monitor },
];

function UserMenu(p: Props & { slim: boolean }): ReactNode {
  const theme = useThemeChoice();
  return (
    <MenuRoot>
      <MenuTrigger
        aria-label={`Account menu for ${p.user.name}`}
        className={cn(
          "flex h-10 w-full items-center gap-2.5 rounded-(--radius-control) px-1.5 hover:bg-surface-2",
          p.slim && "md:justify-center",
        )}
      >
        <span
          aria-hidden="true"
          className="flex size-7 shrink-0 items-center justify-center rounded-full bg-accent-soft text-xs font-semibold"
        >
          {initials(p.user.name)}
        </span>
        <span className={cn("flex-1 truncate text-left", p.slim && "md:hidden")}>
          {p.user.name}
        </span>
      </MenuTrigger>
      <MenuContent side="top" align="start">
        <MenuLabel>{p.user.email}</MenuLabel>
        <MenuItem asChild>
          <Link to="/w/$ws/settings" params={{ ws: p.ws }} onClick={p.onCloseDrawer}>
            <Settings size={16} aria-hidden="true" /> Settings
          </Link>
        </MenuItem>
        <MenuSeparator />
        <MenuLabel>Theme</MenuLabel>
        <MenuRadioGroup value={theme} onValueChange={(v) => p.onChangeTheme(v as ThemeChoice)}>
          {THEMES.map((t) => (
            <MenuRadioItem key={t.value} value={t.value}>
              <span className="flex items-center gap-2">
                <t.icon size={16} aria-hidden="true" /> {t.label}
              </span>
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>
        <MenuSeparator />
        <MenuItem asChild>
          <Link to="/w/$ws/status" params={{ ws: p.ws }} onClick={p.onCloseDrawer}>
            Status
          </Link>
        </MenuItem>
        <MenuItem onSelect={p.onLogout}>
          <LogOut size={16} aria-hidden="true" /> Log out
        </MenuItem>
      </MenuContent>
    </MenuRoot>
  );
}
