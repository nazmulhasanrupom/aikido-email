import { useQueryClient } from "@tanstack/react-query";
import { Outlet, useNavigate, useRouter } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { type AuthState, logout, saveThemePreference } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { readStored, writeStored } from "@/lib/safe-storage";
import { applyChoice, type ThemeChoice } from "@/theme/theme";
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

  // The theme of the account follows the user to a new browser.
  const saved = user?.preferences?.theme;
  useEffect(() => {
    if (saved === "light" || saved === "dark" || saved === "system") applyChoice(saved);
  }, [saved]);

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
        <TopBar onOpenDrawer={() => setDrawer(true)} />
        <main id="content" className="min-h-0 flex-1 overflow-auto">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
