import { createFileRoute, Outlet } from "@tanstack/react-router";
import { LinkTabs, Page, PageHeader } from "@/components/ui";
import { useUser } from "@/features/settings/useUser";

export const Route = createFileRoute("/w/$ws/settings")({ component: SettingsLayout });

function SettingsLayout() {
  const { ws } = Route.useParams();
  const { isAdmin } = useUser();
  const p = { ws };
  return (
    <Page>
      <PageHeader title="Settings" />
      <LinkTabs
        label="Settings pages"
        items={[
          { to: "/w/$ws/settings/account", params: p, label: "Account" },
          ...(isAdmin ? [{ to: "/w/$ws/settings/api", params: p, label: "API" }] : []),
          { to: "/w/$ws/settings/notifications", params: p, label: "Notifications" },
          { to: "/w/$ws/settings/integrations", params: p, label: "Integrations" },
          ...(isAdmin
            ? [
                { to: "/w/$ws/settings/team", params: p, label: "Team" },
                { to: "/w/$ws/settings/sending", params: p, label: "Sending" },
              ]
            : []),
        ]}
      />
      <Outlet />
    </Page>
  );
}
