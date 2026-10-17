import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, redirect } from "@tanstack/react-router";
import { authStateQuery } from "@/api/auth";
import { Shell } from "@/components/shell/Shell";

/** Everything inside a workspace needs a login. In v1 the only workspace is `default`. */
export const Route = createFileRoute("/w/$ws")({
  beforeLoad: async ({ context, params, location }) => {
    const state = await context.queryClient.ensureQueryData(authStateQuery);
    if (state.setup_needed) throw redirect({ to: "/setup" });
    if (!state.logged_in) throw redirect({ to: "/login", search: { next: location.href } });
    if (params.ws !== "default")
      throw redirect({ to: "/w/$ws/campaigns", params: { ws: "default" } });
  },
  component: Workspace,
});

function Workspace() {
  const { ws } = Route.useParams();
  const { data } = useSuspenseQuery(authStateQuery);
  return <Shell ws={ws} state={data} />;
}
