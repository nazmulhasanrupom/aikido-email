import { createFileRoute } from "@tanstack/react-router";
import { Activity } from "lucide-react";
import { ComingSoon } from "@/components/ComingSoon";

export const Route = createFileRoute("/w/$ws/status")({ component: Page });

function Page() {
  return (
    <ComingSoon
      icon={Activity}
      title="Status"
      text="The state of the server and the engines. This page is built in a later phase."
    />
  );
}
