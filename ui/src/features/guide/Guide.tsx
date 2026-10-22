import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { ChevronRight } from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { q } from "@/api/queries";
import { Button, Card, CopyButton, Page, PageHeader, Pill } from "@/components/ui";
import { useUser } from "@/features/settings/useUser";
import { cn } from "@/lib/cn";
import { ALL_IDS, initialOpen, setOpen } from "./open";
import { type GuideSection, SECTIONS } from "./sections";

/** `text` with `code` between backticks. */
function Inline({ text }: { text: string }) {
  return (
    <>
      {text.split("`").map((part, i) =>
        i % 2 === 1 ? (
          <code key={i} className="rounded bg-surface-2 px-1 py-0.5 font-mono text-[0.85em]">
            {part}
          </code>
        ) : (
          <span key={i}>{part}</span>
        ),
      )}
    </>
  );
}

/** The step-by-step help for every credential. Each part folds open and closed. */
export function Guide({ ws, hash }: { ws: string; hash?: string }) {
  const [open, setOpenSet] = useState<Set<string>>(() => initialOpen(hash));
  const done = useDone();

  // The part named in the address opens and scrolls into view.
  const scrolled = useRef<string | null>(null);
  useEffect(() => {
    if (!hash || !SECTIONS.some((s) => s.id === hash)) return;
    setOpenSet((o) => (o.has(hash) ? o : setOpen(o, hash, true)));
    if (scrolled.current === hash) return;
    scrolled.current = hash;
    requestAnimationFrame(() =>
      document.getElementById(hash)?.scrollIntoView?.({ block: "start" }),
    );
  }, [hash]);

  function toggle(id: string, isOpen: boolean) {
    setOpenSet((o) => setOpen(o, id, isOpen));
  }

  return (
    <Page>
      <PageHeader
        title="Guide"
        sub="Step-by-step help for every key and setting. Open a part to read it."
        actions={
          <>
            <Button variant="secondary" onClick={() => setOpenSet(new Set(ALL_IDS))}>
              Open all
            </Button>
            <Button variant="secondary" onClick={() => setOpenSet(new Set())}>
              Close all
            </Button>
          </>
        }
      />
      <div className="flex flex-col gap-3">
        {SECTIONS.map((s) => (
          <Section
            key={s.id}
            ws={ws}
            section={s}
            open={open.has(s.id)}
            done={done[s.id] === true}
            onToggle={(isOpen) => toggle(s.id, isOpen)}
          />
        ))}
      </div>
    </Page>
  );
}

function Section({
  ws,
  section: s,
  open,
  done,
  onToggle,
}: {
  ws: string;
  section: GuideSection;
  open: boolean;
  done: boolean;
  onToggle: (open: boolean) => void;
}) {
  return (
    <Card className="p-0">
      <details
        id={s.id}
        open={open}
        onToggle={(e) => onToggle(e.currentTarget.open)}
        className="group scroll-mt-20"
      >
        <summary className="flex cursor-pointer list-none items-center gap-3 rounded-(--radius-card) px-5 py-4 [&::-webkit-details-marker]:hidden">
          <ChevronRight
            size={16}
            aria-hidden="true"
            className="shrink-0 text-muted transition-transform group-open:rotate-90"
          />
          <h2 className="flex-1 font-semibold">{s.title}</h2>
          {done && <Pill tone="success">Done</Pill>}
        </summary>
        <div className="flex flex-col gap-4 border-t border-border px-5 py-4 text-sm">
          <p>
            <Inline text={s.about} />
          </p>
          {s.live && <Live kind={s.live} />}
          {s.need.length > 0 && (
            <Block title="What you need">
              <ul className="list-disc pl-5">
                {s.need.map((n) => (
                  <li key={n}>
                    <Inline text={n} />
                  </li>
                ))}
              </ul>
            </Block>
          )}
          {s.steps.length > 0 && (
            <Block title="Steps">
              <ol className="list-decimal pl-5">
                {s.steps.map((t) => (
                  <li key={t} className="mb-1">
                    <Inline text={t} />
                  </li>
                ))}
              </ol>
            </Block>
          )}
          {s.errors.length > 0 && (
            <Block title={s.id === "trouble" ? "Problem and fix" : "Common errors"}>
              <dl className="flex flex-col gap-2">
                {s.errors.map((e) => (
                  <div key={e.problem}>
                    <dt className="font-medium">
                      <Inline text={e.problem} />
                    </dt>
                    <dd className="text-muted">
                      <Inline text={e.fix} />
                    </dd>
                  </div>
                ))}
              </dl>
            </Block>
          )}
          {s.link && <GuideLink ws={ws} to={s.link.to} label={s.link.label} />}
        </div>
      </details>
    </Card>
  );
}

function Block({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-1.5">
      <h3 className={cn("text-xs font-semibold uppercase tracking-wider text-muted")}>{title}</h3>
      {children}
    </section>
  );
}

function GuideLink({
  ws,
  to,
  label,
}: {
  ws: string;
  to: NonNullable<GuideSection["link"]>["to"];
  label: string;
}) {
  const cls = "text-accent underline";
  switch (to) {
    case "environment":
      return (
        <Link to="/w/$ws/settings/environment" params={{ ws }} className={cls}>
          {label}
        </Link>
      );
    case "accounts":
      return (
        <Link to="/w/$ws/accounts" params={{ ws }} className={cls}>
          {label}
        </Link>
      );
    case "integrations":
      return (
        <Link to="/w/$ws/settings/integrations" params={{ ws }} className={cls}>
          {label}
        </Link>
      );
    case "api":
      return (
        <Link to="/w/$ws/settings/api" params={{ ws }} className={cls}>
          {label}
        </Link>
      );
    case "leads":
      return (
        <Link to="/w/$ws/leads" params={{ ws }} className={cls}>
          {label}
        </Link>
      );
  }
}

/** A value of this server, to copy. */
function Live({ kind }: { kind: NonNullable<GuideSection["live"]> }) {
  const config = useQuery(q.config).data;
  const [label, value] =
    kind === "redirect-google"
      ? ["Redirect address for Google", config?.redirect_uris.gmail]
      : kind === "redirect-microsoft"
        ? ["Redirect address for Microsoft", config?.redirect_uris.microsoft]
        : ["Public address", config?.redirect_uris.gmail?.replace(/\/v1\/accounts.*$/, "")];
  return (
    <div className="flex flex-col gap-1 rounded-(--radius-control) border border-border bg-surface-2 p-3">
      <span className="text-xs font-medium text-muted">{label}</span>
      <div className="flex items-center gap-2">
        <code className="min-w-0 flex-1 truncate font-mono text-xs">
          {value ?? "Not set. Set AIKIDO_PUBLIC_URL in .env first."}
        </code>
        {value && <CopyButton text={value} />}
      </div>
    </div>
  );
}

/** Which parts are already done on this server. */
function useDone(): Record<string, boolean> {
  const { isAdmin } = useUser();
  const env = useQuery({ ...q.environment, enabled: isAdmin }).data;
  const onboarding = useQuery(q.onboarding).data;
  const config = useQuery(q.config).data;
  const set = (...keys: string[]) =>
    !!env && keys.every((k) => env.items.find((i) => i.key === k)?.is_set);
  const accounts = (onboarding?.facts.accounts ?? 0) > 0;
  return {
    start: accounts,
    google: set("GOOGLE_CLIENT_ID", "GOOGLE_CLIENT_SECRET"),
    microsoft: set("MS_CLIENT_ID", "MS_CLIENT_SECRET"),
    imap: accounts,
    ai: (onboarding?.facts.providers ?? 0) > 0,
    url: config?.public_url_set === true,
  };
}
