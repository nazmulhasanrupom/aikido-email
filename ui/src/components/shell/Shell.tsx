import { useQueryClient } from "@tanstack/react-query";
import { Outlet, useNavigate, useRouter } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { type AuthState, logout, saveThemePreference } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { readStored, writeStored } from "@/lib/safe-storage";
import { applyChoice, isChoice, type ThemeChoice } from "@/theme/theme";
import { DevPanel } from "@/features/developer/DevPanel";
import { useDeveloperMode } from "@/features/developer/api";
import { Banners } from "./Banners";
import { Palette } from "./Palette";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";

const SIDEBAR_KEY = "aikido.sidebar";

/** The frame of the app: menu on the left, top bar, and the page. */
export function Shell({ ws, state }: { ws: string; state: AuthState }) {
  const user = state.user;
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const router = useRouter();
  const [collapsed, setCollapsed] = useState(() => readStored(SIDEBAR_KEY) === "collapsed");
  const [drawer, setDrawer] = useState(false);
  const [search, setSearch] = useState(false);
  const developer = useDeveloperMode();

  // The theme of the account follows the user to a new browser.
  const saved = user?.preferences?.theme;
  useEffect(() => {
    if (isChoice(saved)) applyChoice(saved);
  }, [saved]);

  // Ctrl/Cmd + K opens the search.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setSearch(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // A page change closes the phone drawer.
  useEffect(() => router.subscribe("onResolved", () => setDrawer(false)), [router]);

  if (!user) return null;

  function toggleCollapsed() {
    setCollapsed((c) => {
      writeStored(SIDEBAR_KEY, c ? "open" : "collapsed");
      return !c;
    });
  }

  async function changeTheme(choice: ThemeChoice) {
    applyChoice(choice);
    try {
      await saveThemePreference(choice);
    } catch (e) {
      toast.error(`The theme is changed here, but not saved to your account. ${messageOf(e)}`);
    }
  }

  async function onLogout() {
    try {
      await logout();
    } catch {
      // the session may be gone already; go to the login page anyway
    }
    queryClient.clear();
    await navigate({ to: "/login", search: {} });
  }

  return (
    <div className="flex h-full">
      <Sidebar
        ws={ws}
        state={state}
        user={user}
        collapsed={collapsed}
        onToggleCollapsed={toggleCollapsed}
        drawerOpen={drawer}
        onCloseDrawer={() => setDrawer(false)}
        onChangeTheme={changeTheme}
        onLogout={onLogout}
      />
      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar onOpenDrawer={() => setDrawer(true)} onOpenSearch={() => setSearch(true)} />
        <Banners ws={ws} />
        <main id="content" className="min-h-0 flex-1 overflow-auto">
          <Outlet />
        </main>
        {developer.on && <DevPanel ws={ws} />}
      </div>
      <Palette open={search} onClose={() => setSearch(false)} ws={ws} />
    </div>
  );
}
