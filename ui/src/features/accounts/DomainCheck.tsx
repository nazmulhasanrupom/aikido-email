import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { http, type S } from "@/api/http";
import { Button, CopyButton, ErrorNote, Pill, Skeleton } from "@/components/ui";

type Check = S["Check"];
const TONE = { ok: "success", warn: "warning", fail: "danger", unknown: "neutral" } as const;
const WORD = { ok: "Good", warn: "Look at this", fail: "Fix this", unknown: "Not known" } as const;

export const dnsQuery = (id: string, selector = "") => ({
  queryKey: ["accounts", id, "dns", selector],
  queryFn: () =>
    http.get<S["DnsReport"]>("/v1/accounts/{id}/dns-check", {
      path: { id },
      query: selector ? { selector } : {},
    }),
  staleTime: 60_000,
});

/** A small chip with the worst result. Used in lists. */
export function DomainChip({ accountId }: { accountId: string }) {
  const r = useQuery(dnsQuery(accountId));
  if (r.isPending) return <span className="text-muted">...</span>;
  if (!r.data) return null;
  const o = r.data.overall as keyof typeof TONE;
  return (
    <span title="SPF, DKIM, DMARC and MX of the sending domain">
      <Pill tone={TONE[o] ?? "neutral"}>{WORD[o] ?? o}</Pill>
    </span>
  );
}

function Row({ name, c }: { name: string; c: Check }) {
  const s = c.status as keyof typeof TONE;
  return (
    <li className="flex flex-col gap-1 border-b border-border py-2 last:border-0">
      <div className="flex items-center gap-2">
        <Pill tone={TONE[s] ?? "neutral"}>{WORD[s] ?? c.status}</Pill>
        <span className="font-medium">{name}</span>
        <span className="text-sm text-muted">{c.summary}</span>
      </div>
      {c.found && <code className="block truncate text-xs text-muted">{c.found}</code>}
      {c.fix && (
        <div className="flex items-start gap-2 rounded bg-surface-2 p-2 text-xs">
          <code className="flex-1 whitespace-pre-wrap break-all">{c.fix}</code>
          <CopyButton text={c.fix} />
        </div>
      )}
    </li>
  );
}

/** SPF, DKIM, DMARC and MX of the sending domain, in green, orange and red, with the record to add. */
export function DomainCheck({ accountId, compact }: { accountId: string; compact?: boolean }) {
  const [typed, setTyped] = useState("");
  const [selector, setSelector] = useState("");
  const r = useQuery(dnsQuery(accountId, selector));
  return (
    <div className="flex flex-col gap-2" aria-label="Domain check">
      {!compact && (
        <div className="flex items-center justify-between gap-2">
          <h2 className="font-semibold">Domain check{r.data ? `: ${r.data.domain}` : ""}</h2>
          <Button variant="secondary" className="h-8" onClick={() => void r.refetch()} busy={r.isFetching}>
            Check again
          </Button>
        </div>
      )}
      {r.isPending && <Skeleton rows={4} />}
      {r.isError && <ErrorNote error={r.error} onRetry={() => void r.refetch()} />}
      {r.data && (
        <ul>
          <Row name="SPF" c={r.data.spf} />
          <Row name="DKIM" c={r.data.dkim} />
          <Row name="DMARC" c={r.data.dmarc} />
          <Row name="Mail server (MX)" c={r.data.mx} />
        </ul>
      )}
      {!compact && (
        <form
          className="flex items-end gap-2 text-sm"
          onSubmit={(e) => {
            e.preventDefault();
            setSelector(typed.trim());
          }}
        >
          <label className="flex flex-col gap-1">
            <span className="text-muted">Your DKIM selector (if it is not a common one)</span>
            <input
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              placeholder="for example s1"
              className="h-8 w-48 rounded border border-border bg-surface-2 px-2"
            />
          </label>
          <Button type="submit" variant="secondary" className="h-8">
            Check
          </Button>
        </form>
      )}
    </div>
  );
}
