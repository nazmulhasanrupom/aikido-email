import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/w/$ws/automations/$id/executions/$runId")({ component: () => null });
