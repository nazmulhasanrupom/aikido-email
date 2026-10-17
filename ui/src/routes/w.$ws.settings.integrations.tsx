import { createFileRoute, Outlet } from "@tanstack/react-router";

export const Route = createFileRoute("/w/$ws/settings/integrations")({ component: Outlet });
