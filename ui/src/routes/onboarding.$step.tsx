import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, redirect, useNavigate } from "@tanstack/react-router";
import { Check } from "lucide-react";
import { useEffect, useState } from "react";
import { authStateQuery } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { type GenProvider, q } from "@/api/queries";
import { Logo } from "@/components/Logo";
import { Button, Card, Check2, ErrorNote, Notice, NumField, Pill, Skeleton } from "@/components/ui";
import { AddAccount } from "@/features/accounts/AddAccount";
import { DomainCheck } from "@/features/accounts/DomainCheck";
import { ProviderForm } from "@/features/integrations/ProviderForm";
import { ImportFlow } from "@/features/leads/ImportFlow";
import { DaysAndHours } from "@/features/settings/SendingForm";
import { cn } from "@/lib/cn";
import { browserTimezone } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/onboarding/$step")({
  beforeLoad: async ({ context }) => {
    const state = await context.queryClient.ensureQueryData(authStateQuery);
    if (!state.logged_in) throw redirect({ to: "/login", search: {} });
  },
  component: Onboarding,
});

const STEPS = [
  { id: "connect-email", label: "Email" },
  { id: "sending-basics", label: "Sending" },
  { id: "ai", label: "AI" },
  { id: "import", label: "Leads" },
  { id: "done", label: "Done" },
] as const;

function Onboarding() {
  const { step } = Route.useParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const onboarding = useQuery(q.onboarding);
  const index = Math.max(
    0,
    STEPS.findIndex((s) => s.id === step),
  );
  const facts = onboarding.data?.facts;

  // remember where the person is, so a reload resumes
  useEffect(() => {
    if (onboarding.data && onboarding.data.state.step !== step) {
      void http.patch("/v1/ui/onboarding", { body: { step } });
    }
  }, [step, onboarding.data]);

  const go = (i: number) =>
    navigate({
      to: "/onboarding/$step",
      params: { step: STEPS[Math.min(i, STEPS.length - 1)]?.id ?? "done" },
    });
  const finish = async () => {
    await http.patch("/v1/ui/onboarding", { body: { done: true, step: "done" } });
    await qc.invalidateQueries({ queryKey: q.onboarding.queryKey });
    await navigate({ to: "/w/$ws/campaigns", params: { ws: "default" } });
  };

  return (
    <main className="mx-auto flex min-h-full max-w-2xl flex-col gap-6 px-4 py-10">
      <div className="flex items-center justify-center gap-3">
        <Logo size={32} />
        <span className="text-lg font-semibold tracking-tight">AIKIDO EMAIL</span>
      </div>
      <ol aria-label="Setup steps" className="flex items-center justify-between gap-2 text-sm">
        {STEPS.map((s, i) => (
          <li
            key={s.id}
            className="flex flex-1 items-center gap-2"
            aria-current={i === index ? "step" : undefined}
          >
            <span
              className={cn(
                "flex size-6 items-center justify-center rounded-full border text-xs",
                i < index
                  ? "border-accent bg-accent text-accent-fg"
                  : i === index
                    ? "border-accent"
                    : "border-border text-muted",
              )}
            >
              {i < index ? <Check size={12} /> : i + 1}
            </span>
            <span className={i === index ? "font-medium" : "text-muted"}>{s.label}</span>
            {i < STEPS.length - 1 && <span className="h-px flex-1 bg-border" aria-hidden="true" />}
          </li>
        ))}
      </ol>
      <Card className="flex flex-col gap-5">
        {onboarding.isPending && <Skeleton rows={3} />}
        {onboarding.isError && <ErrorNote error={onboarding.error} />}
        {facts && step === "connect-email" && <EmailStep facts={facts} next={() => go(1)} />}
        {facts && step === "sending-basics" && (
          <SendingStep back={() => go(0)} next={() => go(2)} />
        )}
        {facts && step === "ai" && <AiStep facts={facts} back={() => go(1)} next={() => go(3)} />}
        {facts && step === "import" && <ImportStep back={() => go(2)} next={() => go(4)} />}
        {step === "done" && <DoneStep finish={finish} back={() => go(3)} />}
      </Card>
    </main>
  );
}

function Nav({
  back,
  next,
  nextLabel = "Next",
  skip,
  disabled,
}: {
  back?: () => void;
  next: () => void;
  nextLabel?: string;
  skip?: boolean;
  disabled?: boolean;
}) {
  return (
    <div className="flex items-center justify-between border-t border-border pt-4">
      {back ? (
        <Button variant="secondary" onClick={back}>
          Back
        </Button>
      ) : (
        <span />
      )}
      <div className="flex gap-2">
        {skip && (
          <Button variant="ghost" onClick={next}>
            Skip for now
          </Button>
        )}
        <Button onClick={next} disabled={disabled}>
          {nextLabel}
        </Button>
      </div>
    </div>
  );
}

function EmailStep({ facts, next }: { facts: S["OnboardingFacts"]; next: () => void }) {
  const accounts = useQuery(q.accounts);
  const [adding, setAdding] = useState(false);
  const list = accounts.data ?? [];
  return (
    <>
      <div>
        <h1 className="text-xl font-semibold">Connect an email account</h1>
        <p className="text-muted">
          This is the inbox your emails are sent from. You need at least one.
        </p>
      </div>
      {list.map((a) => (
        <div
          key={a.id}
          className="flex flex-col gap-1 rounded-(--radius-control) border border-border p-3"
        >
          <div className="flex items-center justify-between">
            <span className="flex items-center gap-2 font-medium">
              <Check size={14} className="text-success" /> {a.email}
            </span>
            <Pill tone={a.can_read ? "success" : "warning"}>
              {a.can_read ? "Reading replies" : "Cannot read replies"}
            </Pill>
          </div>
          <DomainCheck accountId={a.id} compact />
          {!a.can_read && (
            <p className="text-sm text-warning">
              We can send, but we cannot see replies. Follow-ups will wait (
              {say("read", a.read_status)}).{" "}
              <Link
                to="/w/$ws/accounts/$id/reply-check"
                params={{ ws: "default", id: a.id }}
                className="underline"
              >
                Fix
              </Link>
            </p>
          )}
        </div>
      ))}
      <div>
        <Button variant={list.length ? "secondary" : "primary"} onClick={() => setAdding(true)}>
          {list.length ? "Add another account" : "Connect account"}
        </Button>
      </div>
      <AddAccount open={adding} onClose={() => setAdding(false)} />
      <Nav next={next} disabled={facts.accounts === 0 && list.length === 0} />
    </>
  );
}

function SendingStep({ back, next }: { back: () => void; next: () => void }) {
  const defaults = useQuery(q.sendingDefaults);
  const accounts = useQuery(q.accounts);
  const [s, setS] = useState<S["SendingSettings"] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (defaults.data && !s)
      setS({
        ...defaults.data.settings,
        timezone:
          defaults.data.settings.timezone === "UTC"
            ? browserTimezone()
            : defaults.data.settings.timezone,
      });
  }, [defaults.data, s]);
  if (!s) return <Skeleton rows={4} />;
  async function save() {
    if (!s) return;
    setBusy(true);
    setError(null);
    try {
      await http.patch("/v1/sending/defaults", { body: s });
      for (const a of accounts.data ?? []) {
        await http.post("/v1/sending/accounts/{id}/settings/reset", { path: { id: a.id } });
      }
      next();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }
  const patch = (p: Partial<S["SendingSettings"]>) => setS({ ...s, ...p });
  return (
    <>
      <div>
        <h1 className="text-xl font-semibold">Sending basics</h1>
        <p className="text-muted">When and how fast your emails go out.</p>
      </div>
      <DaysAndHours value={s} onChange={patch} />
      <NumField
        label="Emails per day per account"
        value={s.daily_limit}
        min={1}
        max={500}
        onChange={(n) => patch({ daily_limit: n })}
      />
      {s.daily_limit > 50 && (
        <Notice tone="warning">More than 50 emails a day from one inbox risks spam filters.</Notice>
      )}
      <Check2
        label="Warm up new inboxes"
        help="Start with a few emails a day and add more each day."
        checked={s.warmup_enabled}
        onChange={(warmup_enabled) => patch({ warmup_enabled })}
      />
      <p className="text-sm text-muted">You can change all of this later per account.</p>
      {error && <Notice tone="danger">{error}</Notice>}
      <Nav back={back} next={save} disabled={busy} />
    </>
  );
}

function AiStep({
  facts,
  back,
  next,
}: {
  facts: S["OnboardingFacts"];
  back: () => void;
  next: () => void;
}) {
  const qc = useQueryClient();
  const providers = useQuery(q.providers);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  async function adopt(p: GenProvider, test: { ok: boolean; detail: string }) {
    setMessage({
      ok: test.ok,
      text: test.ok ? "The model answers." : test.detail || "The test failed.",
    });
    if (!test.ok) return;
    await http.patch("/v1/generator/settings", {
      body: { slot_provider_id: p.id, draft_provider_id: p.id },
    });
    await http.patch("/v1/leads/settings", { body: { ai_mapping_provider_id: p.id } });
    await qc.invalidateQueries({ queryKey: ["generator"] });
  }
  const have = (providers.data?.length ?? 0) > 0 || facts.providers > 0;
  return (
    <>
      <div>
        <h1 className="text-xl font-semibold">
          Connect an AI model <span className="text-sm font-normal text-muted">(optional)</span>
        </h1>
        <p className="text-muted">
          The AI writes the personal parts of your emails and maps the columns of your CSV files.
        </p>
      </div>
      {have && (
        <Notice tone="success">
          A model is set up: {providers.data?.map((p) => p.name).join(", ")}
        </Notice>
      )}
      {message && <Notice tone={message.ok ? "success" : "danger"}>{message.text}</Notice>}
      {!have && <ProviderForm onSaved={adopt} />}
      <Nav back={back} next={next} skip={!have} />
    </>
  );
}

function ImportStep({ back, next }: { back: () => void; next: () => void }) {
  return (
    <>
      <div>
        <h1 className="text-xl font-semibold">
          Import leads <span className="text-sm font-normal text-muted">(optional)</span>
        </h1>
        <p className="text-muted">Upload a CSV file. The columns are matched for you.</p>
      </div>
      <ImportFlow compact />
      <Nav back={back} next={next} skip />
    </>
  );
}

function DoneStep({ finish, back }: { finish: () => Promise<void>; back: () => void }) {
  const [busy, setBusy] = useState(false);
  return (
    <>
      <div>
        <h1 className="text-xl font-semibold">You are ready</h1>
        <p className="text-muted">Here is where to go next.</p>
      </div>
      <div className="grid gap-3 sm:grid-cols-3">
        <Link
          to="/w/$ws/campaigns"
          params={{ ws: "default" }}
          className="rounded-(--radius-card) border border-border p-3 hover:bg-surface-2"
        >
          Create your first campaign
        </Link>
        <Link
          to="/w/$ws/leads"
          params={{ ws: "default" }}
          className="rounded-(--radius-card) border border-border p-3 hover:bg-surface-2"
        >
          Go to Leads
        </Link>
        <a
          href="/docs"
          target="_blank"
          rel="noreferrer"
          className="rounded-(--radius-card) border border-border p-3 hover:bg-surface-2"
        >
          Read the docs
        </a>
      </div>
      <div className="flex justify-between border-t border-border pt-4">
        <Button variant="secondary" onClick={back}>
          Back
        </Button>
        <Button
          busy={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await finish();
            } finally {
              setBusy(false);
            }
          }}
        >
          Go to dashboard
        </Button>
      </div>
    </>
  );
}
