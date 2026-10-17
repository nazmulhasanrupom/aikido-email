import { createFileRoute } from "@tanstack/react-router";
import { Send } from "lucide-react";
import { ComingSoon } from "@/components/ComingSoon";

export const Route = createFileRoute("/w/$ws/campaigns")({ component: Page });

function Page() {
  return (
    <ComingSoon
      icon={Send}
      title="Campaigns"
      text="Campaigns send a set of emails to your leads, one by one. This page is built in a later phase."
    />
  );
}
