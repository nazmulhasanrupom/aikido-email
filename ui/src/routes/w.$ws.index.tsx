import { createFileRoute, Navigate, redirect } from "@tanstack/react-router";
import { Home } from "@/features/home/Home";
import { currentDesign, useDesign } from "@/theme/theme";

/** The start page: Home in the Aikido design. The other designs open on the campaigns. */
export const Route = createFileRoute("/w/$ws/")({
  beforeLoad: ({ params }) => {
    if (currentDesign() !== "aikido") throw redirect({ to: "/w/$ws/campaigns", params });
  },
  component: HomeRoute,
});

function HomeRoute() {
  const { ws } = Route.useParams();
  const design = useDesign();
  if (design !== "aikido") return <Navigate to="/w/$ws/campaigns" params={{ ws }} replace />;
  return <Home ws={ws} />;
}
