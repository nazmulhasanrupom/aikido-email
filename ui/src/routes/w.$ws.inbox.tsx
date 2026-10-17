import { createFileRoute } from "@tanstack/react-router";
import { Inbox } from "lucide-react";
import { ComingSoon } from "@/components/ComingSoon";

export const Route = createFileRoute("/w/$ws/inbox")({ component: Page });

function Page() {
  return (
    <ComingSoon
      icon={Inbox}
      title="Inbox"
      text="The replies we find in your mailboxes. This page is built in a later phase."
    />
  );
}
