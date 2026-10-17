import type { S } from "@/api/http";
import { Pill } from "@/components/ui";
import { say } from "@/lib/words";

export function AccountStatusPill({
  status,
  paused,
}: {
  status: S["AccountStatus"];
  paused?: boolean | undefined;
}) {
  const tone =
    status === "active" && !paused
      ? "success"
      : status === "paused" || paused
        ? "warning"
        : "danger";
  return (
    <Pill tone={tone}>{paused && status === "active" ? "Paused" : say("account", status)}</Pill>
  );
}

export function ReadingCell({ account }: { account: S["AccountView"] }) {
  if (account.can_read) return <Pill tone="success">Reading</Pill>;
  return (
    <span className="text-warning" title={account.last_error ?? undefined}>
      Cannot read: {say("read", account.read_status)}
    </span>
  );
}
