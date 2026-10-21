import { Pill } from "@/components/ui";

/** The open state of a lead: Opened, Maybe opened (only proxy opens) or nothing. */
export function OpenPill({ status, count }: { status?: string | null; count?: number | null }) {
  if (status === "opened") {
    return (
      <Pill tone="success" dot>
        Opened{count && count > 1 ? ` x${count}` : ""}
      </Pill>
    );
  }
  if (status === "maybe_opened") {
    return (
      <span title="Only Apple Mail Privacy Protection, the Google image proxy or a scanner loaded the tracker">
        <Pill tone="warning">Maybe opened</Pill>
      </span>
    );
  }
  return null;
}
