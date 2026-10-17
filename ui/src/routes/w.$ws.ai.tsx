import { createFileRoute } from "@tanstack/react-router";
import { Sparkles } from "lucide-react";
import { ComingSoon } from "@/components/ComingSoon";

export const Route = createFileRoute("/w/$ws/ai")({ component: Page });

function Page() {
  return (
    <ComingSoon
      icon={Sparkles}
      title="Aikido AI is coming soon."
      text="Ask questions about your leads and campaigns, and let the AI handle replies."
    />
  );
}
