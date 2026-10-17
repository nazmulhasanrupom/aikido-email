import { createFileRoute } from "@tanstack/react-router";
import { Users } from "lucide-react";
import { ComingSoon } from "@/components/ComingSoon";

export const Route = createFileRoute("/w/$ws/leads")({ component: Page });

function Page() {
  return (
    <ComingSoon
      icon={Users}
      title="Leads"
      text="Lists, tags, custom fields and imports. This page is built in a later phase."
    />
  );
}
