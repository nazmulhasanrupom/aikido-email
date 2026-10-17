import { createFileRoute } from "@tanstack/react-router";
import { Mail } from "lucide-react";
import { ComingSoon } from "@/components/ComingSoon";

export const Route = createFileRoute("/w/$ws/accounts")({ component: Page });

function Page() {
  return (
    <ComingSoon
      icon={Mail}
      title="Email Accounts"
      text="Connect and manage the mailboxes that send. This page is built in a later phase."
    />
  );
}
