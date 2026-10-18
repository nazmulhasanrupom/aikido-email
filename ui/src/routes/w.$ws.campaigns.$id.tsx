import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, Outlet, useNavigate } from "@tanstack/react-router";
import { MoreHorizontal } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { MenuContent, MenuItem, MenuRoot, MenuTrigger } from "@/components/shell/menu";
import {
  Button,
  ConfirmDialog,
  ErrorNote,
  LinkTabs,
  Notice,
  Page,
  Pill,
  Skeleton,
} from "@/components/ui";
import { ChecklistBar } from "@/features/campaigns/ChecklistBar";
import { campaignQuery, tone } from "@/features/campaigns/hooks";
import { LaunchDialog } from "@/features/campaigns/Launch";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/campaigns/$id")({ component: CampaignLayout });

function CampaignLayout() {
  const { ws, id } = Route.useParams();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const res = useQuery(campaignQuery(id));
  const [name, setName] = useState("");
  const [launch, setLaunch] = useState(false);
  const [archive, setArchive] = useState(false);
  useEffect(() => {
    if (res.data) setName(res.data.name);
  }, [res.data]);
  const act = useMutation({
    mutationFn: (what: "pause" | "resume") =>
      http.post(`/v1/campaigns/{id}/${what}`, { path: { id } }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["campaigns"] }),
    onError: (e) => toast.error(messageOf(e)),
  });
  const copy = useMutation({
    mutationFn: () => http.post<{ id: string }>("/v1/campaigns/{id}/duplicate", { path: { id } }),
    onSuccess: async (r) => {
      await qc.invalidateQueries({ queryKey: ["campaigns"] });
      toast.success("Copied. The copy is a draft with no leads.");
      await navigate({ to: "/w/$ws/campaigns/$id/editor", params: { ws, id: r.id } });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const rename = useMutation({
    mutationFn: (n: string) =>
      http.patch("/v1/campaigns/{id}", { path: { id }, body: { name: n } }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["campaigns"] }),
    onError: (e) => {
      toast.error(messageOf(e));
      if (res.data) setName(res.data.name);
    },
  });
  if (res.isPending)
    return (
      <Page>
        <Skeleton />
      </Page>
    );
  if (res.isError)
    return (
      <Page>
        <ErrorNote error={res.error} />
        <Link to="/w/$ws/campaigns" params={{ ws }} className="text-accent underline">
          Back to campaigns
        </Link>
      </Page>
    );
  const c = res.data;
  const p = { ws, id };
  return (
    <Page wide>
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <input
            aria-label="Campaign name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onBlur={() => name.trim() && name !== c.name && rename.mutate(name.trim())}
            onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
            className="min-w-0 rounded-(--radius-control) border border-transparent bg-transparent px-2 py-1 text-xl font-semibold hover:border-border focus:border-border"
          />
          <Pill tone={tone(c.status)}>{say("campaign", c.status)}</Pill>
        </div>
        <div className="flex items-center gap-2">
          {c.status === "draft" && <Button onClick={() => setLaunch(true)}>Launch</Button>}
          {c.status === "active" && (
            <Button variant="secondary" busy={act.isPending} onClick={() => act.mutate("pause")}>
              Pause
            </Button>
          )}
          {c.status === "paused" && (
            <Button busy={act.isPending} onClick={() => act.mutate("resume")}>
              Resume
            </Button>
          )}
          <MenuRoot>
            <MenuTrigger aria-label="More actions" className="rounded p-1.5 hover:bg-surface-2">
              <MoreHorizontal size={18} />
            </MenuTrigger>
            <MenuContent align="end">
              <MenuItem onSelect={() => copy.mutate()}>Duplicate</MenuItem>
              <MenuItem onSelect={() => setArchive(true)}>Archive</MenuItem>
            </MenuContent>
          </MenuRoot>
        </div>
      </div>
      {c.warning && (
        <div className="mb-3">
          <Notice tone="warning">{c.warning}</Notice>
        </div>
      )}
      {(c.status === "draft" || c.status === "active" || c.status === "paused") && <ChecklistBar campaignId={id} ws={ws} />}
      <LinkTabs
        label="Campaign pages"
        items={[
          { to: "/w/$ws/campaigns/$id/editor", params: p, label: "Editor" },
          { to: "/w/$ws/campaigns/$id/leads", params: p, label: "Leads" },
          { to: "/w/$ws/campaigns/$id/followups", params: p, label: "Follow-ups" },
          { to: "/w/$ws/campaigns/$id/settings", params: p, label: "Settings" },
          { to: "/w/$ws/campaigns/$id/reports", params: p, label: "Reports" },
        ]}
      />
      <Outlet />
      <LaunchDialog open={launch} onClose={() => setLaunch(false)} campaign={c} ws={ws} />
      <ConfirmDialog
        open={archive}
        onClose={() => setArchive(false)}
        title="Archive this campaign?"
        typed={c.name}
        confirmLabel="Archive campaign"
        text="Pending leads are skipped and the sequence stops."
        onConfirm={async () => {
          await http.post("/v1/campaigns/{id}/archive", { path: { id } });
          await qc.invalidateQueries({ queryKey: ["campaigns"] });
        }}
      />
    </Page>
  );
}
