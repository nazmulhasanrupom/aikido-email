import { createFileRoute, Outlet } from "@tanstack/react-router";

export const Route = createFileRoute("/w/$ws/automations")({ component: Outlet });
