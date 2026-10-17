import { createFileRoute } from "@tanstack/react-router";
import { ChartColumn } from "lucide-react";
import { ComingSoon } from "@/components/ComingSoon";

export const Route = createFileRoute("/w/$ws/reports")({ component: Page });

function Page() {
  return (
    <ComingSoon
      icon={ChartColumn}
      title="Reports"
      text="Numbers and charts. This page is built in a later phase."
    />
  );
}
