import { createFileRoute, Outlet } from "@tanstack/react-router";

export const Route = createFileRoute("/w/$ws/accounts")({ component: Outlet });
