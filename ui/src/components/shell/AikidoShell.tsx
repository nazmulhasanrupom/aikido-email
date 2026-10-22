import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Outlet, useRouterState } from "@tanstack/react-router";
import {
  Activity,
  BookOpen,
  ChevronDown,
  LogOut,
  Mail,
  Menu as MenuIcon,
  Plus,
  Search,
  Send,
  Settings,
  Upload,
} from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import type { AuthState, User } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { AddAccount } from "@/features/accounts/AddAccount";
import { NewCampaign } from "@/features/campaigns/NewCampaign";
import { DevPanel } from "@/features/developer/DevPanel";
import { useDeveloperMode } from "@/features/developer/api";
import { ImportModal } from "@/features/leads/ImportModal";
import { useReoon } from "@/features/reoon/api";
import { useUser } from "@/features/settings/useUser";
import { cn } from "@/lib/cn";
import { fmtInt } from "@/lib/format";
import type { DesignChoice, ThemeChoice } from "@/theme/theme";
import { Logo } from "../Logo";
import { InspectorContext } from "../ui";
import { Banners } from "./Banners";
import {
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuRoot,
  MenuSeparator,
  MenuTrigger,
} from "./menu";
import { initials, LookItems, useUnreadReplies, WorkspaceSettings } from "./Sidebar";
import { AlertsMenu, isMac } from "./TopBar";

export interface FrameProps {
  ws: string;
  state: AuthState;
  user: User;
  onOpenSearch: () => void;
  onChangeTheme: (choice: ThemeChoice) => void;
  onChangeDesign: (design: DesignChoice) => void;
  onLogout: () => void;
}

interface NavItem {
  to:
    | "/w/$ws"
    | "/w/$ws/campaigns"
    | "/w/$ws/leads"
    | "/w/$ws/inbox"
    | "/w/$ws/accounts"
    | "/w/$ws/personalization"
    | "/w/$ws/reports";
  label: string;
  exact?: boolean;
}

const NAV: NavItem[] = [
  { to: "/w/$ws", label: "Home", exact: true },
  { to: "/w/$ws/campaigns", label: "Campaigns" },
  { to: "/w/$ws/leads", label: "Leads" },
  { to: "/w/$ws/inbox", label: "Inbox" },
  { to: "/w/$ws/accounts", label: "Accounts" },
  { to: "/w/$ws/personalization", label: "Personalisation" },
  { to: "/w/$ws/reports", label: "Reports" },
];

/** A page link of the top bar. The page you are on is white with a red line under it. */
const NAV_LINK =
  "relative flex h-full items-center gap-1.5 px-2.5 text-[13px] font-medium text-(--topnav-muted) transition-colors hover:text-(--topnav-fg) after:absolute after:inset-x-2.5 after:bottom-0 after:h-0.5 after:rounded-full after:bg-transparent data-[status=active]:text-(--topnav-fg) data-[status=active]:after:bg-(--topnav-mark) data-[state=open]:text-(--topnav-fg)";

const ICON_BUTTON =
  "relative flex size-8 shrink-0 items-center justify-center rounded-(--radius-control) text-(--topnav-muted) hover:bg-(--topnav-hover) hover:text-(--topnav-fg) data-[state=open]:bg-(--topnav-hover) data-[state=open]:text-(--topnav-fg) data-[status=active]:bg-(--topnav-hover) data-[status=active]:text-(--topnav-fg)";

/**
 * The frame of the Aikido design: the pages in a dark bar on top, the page, a side panel on the right
 * (lead, email and preview details open there, next to the page) and a status bar at the bottom.
 */
export function AikidoShell(p: FrameProps) {
  const developer = useDeveloperMode();
  // the element of the side panel; a `side` Modal is drawn into it
  const [dock, setDock] = useState<HTMLElement | null>(null);
  return (
    <InspectorContext.Provider value={dock}>
      <div className="flex h-full flex-col">
        <TopNav {...p} />
        {/* the status bar shows these on wide screens */}
        <div className="md:hidden">
          <Banners ws={p.ws} />
        </div>
        <div className="flex min-h-0 flex-1">
          <main id="content" className="min-w-0 flex-1 overflow-auto">
            <Outlet />
          </main>
          <aside
            ref={setDock}
            aria-label="Details"
            className="ak-inspector fixed inset-y-0 right-0 z-40 w-full max-w-md border-l border-border bg-surface shadow-(--shadow-pop) empty:hidden lg:static lg:z-auto lg:w-[min(460px,38vw)] lg:max-w-none lg:shadow-none"
          />
        </div>
        {developer.on && <DevPanel ws={p.ws} />}
        <StatusBar ws={p.ws} />
      </div>
    </InspectorContext.Provider>
  );
}

function TopNav(p: FrameProps) {
  const inbox = useUnreadReplies(p.user);
  return (
    <header className="ak-topnav flex h-12 shrink-0 items-center gap-1 bg-(--topnav-bg) px-2 text-(--topnav-fg) sm:px-3">
      <PhoneMenu ws={p.ws} inbox={inbox} />
      <WorkspaceButton name={p.state.workspace_name} logo={p.state.workspace_logo} />
      <span aria-hidden="true" className="mx-1.5 hidden h-5 w-px bg-(--topnav-border) lg:block" />
      <nav aria-label="Pages" className="hidden h-full items-stretch lg:flex">
        {NAV.map((item) => (
          <Link
            key={item.to}
            to={item.to}
            params={{ ws: p.ws }}
            activeOptions={{ exact: item.exact ?? false }}
            className={NAV_LINK}
          >
            {item.label}
            {item.to === "/w/$ws/inbox" && inbox > 0 && <Count n={inbox} />}
          </Link>
        ))}
        <MoreMenu ws={p.ws} />
      </nav>
      <div className="ml-auto flex items-center gap-0.5">
        <button
          type="button"
          onClick={p.onOpenSearch}
          aria-label="Search"
          className="flex h-8 items-center gap-2 rounded-(--radius-control) px-2 text-[13px] text-(--topnav-muted) hover:bg-(--topnav-hover) hover:text-(--topnav-fg) xl:mr-1 xl:w-52 xl:border xl:border-(--topnav-border) xl:bg-(--topnav-hover) xl:px-2.5"
        >
          <Search size={15} aria-hidden="true" />
          <span className="hidden flex-1 text-left xl:block">Search</span>
          <kbd className="hidden rounded border border-(--topnav-border) px-1 font-mono text-[10px] xl:block">
            {isMac ? "⌘K" : "Ctrl K"}
          </kbd>
        </button>
        <NewMenu ws={p.ws} />
        <AlertsMenu ink className={ICON_BUTTON} />
        <AccountMenu {...p} />
      </div>
    </header>
  );
}

/** The number of new replies, on the Inbox link. */
function Count({ n }: { n: number }) {
  return (
    <>
      <span
        aria-hidden="true"
        className="rounded-full bg-(--sidebar-badge) px-1.5 text-[10px] leading-4 font-semibold text-(--sidebar-badge-fg)"
      >
        {n > 99 ? "99+" : n}
      </span>
      <span className="sr-only">, {n} new replies</span>
    </>
  );
}

function WorkspaceButton({ name, logo }: { name: string; logo?: string | null | undefined }) {
  const [editing, setEditing] = useState(false);
  const { isAdmin } = useUser();
  return (
    <>
      <WorkspaceSettings open={editing} onClose={() => setEditing(false)} name={name} logo={logo} />
      <MenuRoot>
        <MenuTrigger
          aria-label={`Workspace ${name}`}
          className="flex h-8 min-w-0 items-center gap-2 rounded-(--radius-control) px-1.5 hover:bg-(--topnav-hover) data-[state=open]:bg-(--topnav-hover)"
        >
          {logo ? (
            <img src={logo} alt="" className="size-6 shrink-0 rounded-md object-cover" />
          ) : (
            <Logo size={24} />
          )}
          <span className="hidden max-w-40 truncate text-[13px] font-semibold sm:block lg:hidden xl:block">
            {name}
          </span>
          <ChevronDown size={13} aria-hidden="true" className="shrink-0 text-(--topnav-muted)" />
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

/** The pages that are used less: the AI page, the apps and the status page. */
function MoreMenu({ ws }: { ws: string }) {
  const reoon = useReoon();
  const path = useRouterState({ select: (s) => s.location.pathname });
  const here = /\/w\/[^/]+\/(ai|apps|status)(\/|$)/.test(path);
  return (
    <MenuRoot>
      <MenuTrigger className={NAV_LINK} data-status={here ? "active" : undefined}>
        More <ChevronDown size={13} aria-hidden="true" />
      </MenuTrigger>
      <MenuContent align="start">
        <MoreItems ws={ws} reoon={reoon.installed} />
      </MenuContent>
    </MenuRoot>
  );
}

function MoreItems({ ws, reoon }: { ws: string; reoon: boolean }) {
  return (
    <>
      <MenuItem asChild>
        <Link to="/w/$ws/ai" params={{ ws }}>
          Aikido AI
          <span className="ml-auto rounded bg-surface-2 px-1.5 py-0.5 text-xs text-muted">Soon</span>
        </Link>
      </MenuItem>
      {reoon && (
        <MenuItem asChild>
          <Link to="/w/$ws/apps/reoon" params={{ ws }}>
            Reoon
          </Link>
        </MenuItem>
      )}
      <MenuItem asChild>
        <Link to="/w/$ws/status" params={{ ws }}>
          Status
        </Link>
      </MenuItem>
    </>
  );
}

/** On a narrow screen the page links are in a menu. */
function PhoneMenu({ ws, inbox }: { ws: string; inbox: number }) {
  const reoon = useReoon();
  return (
    <MenuRoot>
      <MenuTrigger aria-label="Open menu" className={cn(ICON_BUTTON, "lg:hidden")}>
        <MenuIcon size={18} aria-hidden="true" />
      </MenuTrigger>
      <MenuContent align="start" className="w-60">
        {NAV.map((item) => (
          <MenuItem key={item.to} asChild>
            <Link
              to={item.to}
              params={{ ws }}
              activeOptions={{ exact: item.exact ?? false }}
              activeProps={{ className: "font-semibold" }}
            >
              {item.label}
              {item.to === "/w/$ws/inbox" && inbox > 0 && (
                <span className="ml-auto text-xs text-muted">{inbox > 99 ? "99+" : inbox} new</span>
              )}
            </Link>
          </MenuItem>
        ))}
        <MenuSeparator />
        <MoreItems ws={ws} reoon={reoon.installed} />
      </MenuContent>
    </MenuRoot>
  );
}

/** Make something new from any page. */
function NewMenu({ ws }: { ws: string }) {
  const [open, setOpen] = useState<null | "campaign" | "leads" | "account">(null);
  const close = () => setOpen(null);
  return (
    <>
      <MenuRoot>
        <MenuTrigger
          aria-label="New"
          className="mx-1 flex h-8 items-center gap-1 rounded-(--radius-control) bg-(--topnav-fg) px-2 text-[13px] font-semibold text-(--topnav-bg) hover:opacity-90 sm:px-2.5"
        >
          <Plus size={15} aria-hidden="true" />
          <span className="hidden sm:inline">New</span>
          <ChevronDown size={13} aria-hidden="true" className="hidden opacity-60 sm:block" />
        </MenuTrigger>
        <MenuContent align="end">
          <MenuItem onSelect={() => setOpen("campaign")}>
            <Send size={15} aria-hidden="true" /> Campaign
          </MenuItem>
          <MenuItem onSelect={() => setOpen("leads")}>
            <Upload size={15} aria-hidden="true" /> Import leads
          </MenuItem>
          <MenuItem onSelect={() => setOpen("account")}>
            <Mail size={15} aria-hidden="true" /> Email account
          </MenuItem>
        </MenuContent>
      </MenuRoot>
      <NewCampaign open={open === "campaign"} onClose={close} ws={ws} />
      <ImportModal open={open === "leads"} onClose={close} />
      <AddAccount open={open === "account"} onClose={close} />
    </>
  );
}

function AccountMenu(p: FrameProps) {
  return (
    <MenuRoot>
      <MenuTrigger
        aria-label={`Account menu for ${p.user.name}`}
        className="ml-1 flex size-8 shrink-0 items-center justify-center rounded-full hover:ring-2 hover:ring-(--topnav-border) data-[state=open]:ring-2 data-[state=open]:ring-(--topnav-mark)"
      >
        <span
          aria-hidden="true"
          className="flex size-7 items-center justify-center rounded-full bg-(--sidebar-avatar) text-[11px] font-semibold text-(--sidebar-avatar-fg)"
        >
          {initials(p.user.name)}
        </span>
      </MenuTrigger>
      <MenuContent align="end" className="w-60">
        <div className="px-2 py-1.5">
          <p className="truncate text-sm font-medium">{p.user.name}</p>
          <p className="truncate text-xs text-muted">{p.user.email}</p>
        </div>
        <MenuSeparator />
        <MenuItem asChild>
          <Link to="/w/$ws/settings/account" params={{ ws: p.ws }}>
            <Settings size={16} aria-hidden="true" /> Settings
          </Link>
        </MenuItem>
        <MenuItem asChild>
          <Link to="/w/$ws/status" params={{ ws: p.ws }}>
            <Activity size={16} aria-hidden="true" /> Status
          </Link>
        </MenuItem>
        <MenuItem asChild>
          <Link to="/w/$ws/guide" params={{ ws: p.ws }}>
            <BookOpen size={16} aria-hidden="true" /> Guide
          </Link>
        </MenuItem>
        <MenuSeparator />
        <LookItems onChangeDesign={p.onChangeDesign} onChangeTheme={p.onChangeTheme} />
        <MenuSeparator />
        <MenuItem onSelect={p.onLogout}>
          <LogOut size={16} aria-hidden="true" /> Log out
        </MenuItem>
      </MenuContent>
    </MenuRoot>
  );
}

/** One line at the bottom: is the engine sending, the queue, the inboxes, the alerts and the version. */
function StatusBar({ ws }: { ws: string }) {
  const { isAdmin } = useUser();
  const qc = useQueryClient();
  const engine = useQuery({
    queryKey: ["status", "sending"],
    queryFn: () => http.get<S["EngineStatus"]>("/v1/sending/engine/status"),
    refetchInterval: 30_000,
  });
  const cfg = useQuery(q.config);
  const alerts = useQuery(q.alerts);
  const resume = useMutation({
    mutationFn: () => http.post("/v1/sending/engine/resume", { body: {} }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["status"] }),
    onError: (e) => toast.error(messageOf(e)),
  });
  const e = engine.data;
  const jobs = e?.jobs ?? {};
  const accounts = e?.accounts ?? {};
  const inboxes = Object.values(accounts).reduce((a, b) => a + b, 0);
  const down = accounts.disconnected ?? 0;
  const open = alerts.data ?? [];
  const errors = open.some((a) => a.severity === "error");
  return (
    <footer
      aria-label="Status"
      className="ak-statusbar hidden h-7 shrink-0 items-center gap-4 border-t border-border bg-(--statusbar-bg) px-3 font-mono text-[11px] text-muted md:flex"
    >
      <span className="flex items-center gap-1.5">
        <span
          aria-hidden="true"
          className={cn(
            "size-1.5 rounded-full",
            !e ? "bg-border" : e.paused ? "bg-warning" : "bg-success",
          )}
        />
        {!e ? "Sending" : e.paused ? "Sending is paused" : "Sending is on"}
        {e?.paused && isAdmin && (
          <button
            type="button"
            disabled={resume.isPending}
            onClick={() => resume.mutate()}
            className="ml-1 rounded-sm px-1 font-medium text-text underline underline-offset-2 hover:bg-surface-2"
          >
            Resume
          </button>
        )}
      </span>
      {(e?.dry_run || cfg.data?.dry_run) && (
        <span className="rounded-sm bg-info/15 px-1.5 text-info">Dry run: emails are logged</span>
      )}
      <span>{fmtInt(jobs.queued)} queued</span>
      {(jobs.sending ?? 0) > 0 && <span>{fmtInt(jobs.sending)} sending</span>}
      {(jobs.stuck ?? 0) > 0 && <span className="text-warning">{fmtInt(jobs.stuck)} stuck</span>}
      <Link to="/w/$ws/accounts" params={{ ws }} className="hover:text-text">
        {fmtInt(inboxes)} {inboxes === 1 ? "inbox" : "inboxes"}
        {down > 0 && <span className="text-danger"> · {fmtInt(down)} disconnected</span>}
      </Link>
      <span className="ml-auto flex items-center gap-4">
        {open.length > 0 && (
          <span className={errors ? "text-danger" : "text-warning"}>
            {open.length} {open.length === 1 ? "alert" : "alerts"}
          </span>
        )}
        <span>{isMac ? "⌘K" : "Ctrl+K"} to search</span>
        {cfg.data && <span>v{cfg.data.version}</span>}
      </span>
    </footer>
  );
}
