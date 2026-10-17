import { createFileRoute } from "@tanstack/react-router";
import { Settings } from "lucide-react";
import { ComingSoon } from "@/components/ComingSoon";

export const Route = createFileRoute("/w/$ws/settings")({ component: Page });

function Page() {
  return (
    <ComingSoon
      icon={Settings}
      title="Settings"
      text="Account, API keys, notifications, integrations, team and sending defaults. This page is built in a later phase."
    />
  );
}
