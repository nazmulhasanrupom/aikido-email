import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { FilePlus2, FileUp, LayoutTemplate } from "lucide-react";
import { type ReactNode, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { Button, ErrorNote, Page, PageHeader, Pill, Skeleton } from "@/components/ui";
import { cn } from "@/lib/cn";
import { aq, call } from "./api";
import { Titled } from "./ConfigPages";
import { CONTROL } from "./ParamForm";
import { ImportDialog } from "./Transfer";
import { keepReport, type TemplateInfo } from "./transfer";

/** `/automations/new`: Blank, From template, Import file (map 17.1). */
export function NewPage({ ws }: { ws: string }) {
  const navigate = useNavigate();
  const [name, setName] = useState("New workflow");
  const [busy, setBusy] = useState(false);
  const [importing, setImporting] = useState(false);
  const [templates, setTemplates] = useState(false);
  const open = (id: string) => navigate({ to: "/w/$ws/automations/$id", params: { ws, id } });

  async function blank() {
    setBusy(true);
    try {
      const w = await call.create({ name: name.trim() || "New workflow" });
      void open(w.id);
    } catch (e) {
      toast.error(messageOf(e));
      setBusy(false);
    }
  }

  return (
    <Page>
      <PageHeader title={<Titled ws={ws}>New workflow</Titled>} sub="Start empty, or bring a file." />
      <div className="grid gap-4 sm:grid-cols-2">
        <Card
          icon={FilePlus2}
          title="Blank"
          text="One manual trigger. Add the rest by dragging nodes."
        >
          <input
            aria-label="Name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            maxLength={120}
            className={CONTROL}
          />
          <Button busy={busy} onClick={() => void blank()}>
            Create
          </Button>
        </Card>
        <Card
          icon={FileUp}
          title="Import a file"
          text="A .json workflow file from AIKIDO EMAIL, or one that an AI wrote."
        >
          <Button variant="secondary" onClick={() => setImporting(true)}>
            Import a file, JSON or link
          </Button>
        </Card>
        <Card
          icon={LayoutTemplate}
          title="From a template"
          text="Ready-made workflows for common jobs."
        >
          <Button
            variant="secondary"
            aria-expanded={templates}
            onClick={() => setTemplates((t) => !t)}
          >
            {templates ? "Hide templates" : "Browse templates"}
          </Button>
        </Card>
      </div>
      {templates && <Templates onOpen={open} />}
      <ImportDialog open={importing} onClose={() => setImporting(false)} ws={ws} />
    </Page>
  );
}

function Card({
  icon: Icon,
  title,
  text,
  children,
  soon,
}: {
  icon: typeof FilePlus2;
  title: string;
  text: string;
  children?: ReactNode;
  soon?: boolean;
}) {
  return (
    <section
      className={cn(
        "flex flex-col gap-3 rounded-(--radius-card) border border-border bg-surface p-4",
        soon && "opacity-60",
      )}
    >
      <div className="flex items-center gap-2">
        <Icon size={18} aria-hidden="true" className="text-muted" />
        <h2 className="font-semibold">{title}</h2>
        {soon && <Pill>Later</Pill>}
      </div>
      <p className="text-sm text-muted">{text}</p>
      {children}
    </section>
  );
}

function Templates({ onOpen }: { onOpen: (id: string) => void }) {
  const list = useQuery(aq.templates);
  const [busy, setBusy] = useState<string | null>(null);

  async function use(t: TemplateInfo) {
    setBusy(t.id);
    try {
      const r = await call.createFromTemplate(t.id, {});
      keepReport(r.workflow.id, r.report);
      toast.success("Made a draft from the template. It is not active.");
      onOpen(r.workflow.id);
    } catch (e) {
      toast.error(messageOf(e));
      setBusy(null);
    }
  }

  return (
    <section aria-label="Templates" className="mt-6">
      <h2 className="mb-3 font-semibold">Templates</h2>
      {list.isPending && <Skeleton rows={3} />}
      {list.isError && <ErrorNote error={list.error} onRetry={() => list.refetch()} />}
      <div className="grid gap-4 sm:grid-cols-2">
        {list.data?.map((t) => (
          <article
            key={t.id}
            className="flex flex-col gap-2 rounded-(--radius-card) border border-border bg-surface p-4"
          >
            <h3 className="font-semibold">{t.name}</h3>
            <p className="text-sm text-muted">{t.description}</p>
            <p className="text-xs text-muted">{t.node_count} nodes</p>
            {t.variables.length > 0 && (
              <p className="text-xs">
                <span className="text-muted">You pick: </span>
                {t.variables.map((v) => v.label || v.key).join(", ")}
              </p>
            )}
            {t.secrets.length > 0 && (
              <p className="text-xs">
                <span className="text-muted">Secrets to add: </span>
                {t.secrets.join(", ")}
              </p>
            )}
            <div className="mt-auto pt-1">
              <Button
                variant="secondary"
                busy={busy === t.id}
                onClick={() => void use(t)}
                aria-label={`Use the template ${t.name}`}
              >
                Use this template
              </Button>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
