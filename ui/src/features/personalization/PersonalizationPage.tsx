import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { FileUp, Globe, Sparkles, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S, upload } from "@/api/http";
import { Button, Card, ErrorNote, Field, Notice, Page, PageHeader, Pill, Skeleton, TextArea } from "@/components/ui";

const profileQuery = { queryKey: ["personalization", "profile"], queryFn: () => http.get<S["ProfileReply"]>("/v1/personalization/profile") };
const sourcesQuery = { queryKey: ["personalization", "sources"], queryFn: () => http.get<S["PersonalizationSource"][]>("/v1/personalization/sources") };

const MAX_CHARS = 4000;

function useRefresh() {
  const qc = useQueryClient();
  return () => qc.invalidateQueries({ queryKey: ["personalization"] });
}

/** Personalize: a business link and a file. The AI turns them into points that prompts use as {{my_business}}. */
export function PersonalizationPage({ ws }: { ws: string }) {
  const profile = useQuery(profileQuery);
  const sources = useQuery(sourcesQuery);
  const refresh = useRefresh();
  const saved = profile.data?.profile?.my_business ?? "";
  const [points, setPoints] = useState("");
  const [isDraft, setIsDraft] = useState(false);

  useEffect(() => {
    if (!isDraft) setPoints(saved);
  }, [saved, isDraft]);

  const generate = useMutation({
    mutationFn: () => http.post<S["BusinessReply"]>("/v1/personalization/business/generate"),
    onSuccess: (d) => {
      setPoints(d.my_business);
      setIsDraft(true);
      toast.success("Points ready. Read them, fix them, then press Save.");
    },
  });
  const save = useMutation({
    mutationFn: () => http.put<S["SenderProfile"]>("/v1/personalization/business", { body: { my_business: points } }),
    onSuccess: async () => {
      setIsDraft(false);
      await refresh();
      toast.success("Saved. Prompts can now use {{my_business}}.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const noProvider = generate.error instanceof ApiProblem && generate.error.code === "generator_not_ready";
  const dirty = points !== saved;

  return (
    <Page>
      <PageHeader title="Personalisation" sub="Give us your business link or a file. The AI writes short points about your business for your emails." />
      {profile.isError && <ErrorNote error={profile.error} onRetry={() => void profile.refetch()} />}

      <div className="mt-4 grid gap-6 lg:grid-cols-2">
        <section aria-label="Sources" className="flex flex-col gap-4">
          <UrlCard />
          <FileCard />
          <Added sources={sources.data} loading={sources.isPending} />
          <div>
            <Button busy={generate.isPending} disabled={!sources.data?.length} onClick={() => generate.mutate()}>
              <Sparkles size={14} /> Generate
            </Button>
          </div>
          {noProvider && (
            <Notice tone="warning">
              Set up an AI provider first.{" "}
              <Link to="/w/$ws/settings/integrations/$id" params={{ ws, id: "ai" }} className="underline">
                Open AI models
              </Link>
            </Notice>
          )}
          {generate.isError && !noProvider && <ErrorNote error={generate.error} />}
        </section>

        <section aria-label="Business points" className="flex flex-col gap-3">
          <h2 className="text-base font-medium">
            Your business points <code className="ml-1 text-sm text-muted">{"{{my_business}}"}</code>
          </h2>
          {isDraft && <Notice tone="info">This is a draft from the AI. Nothing is saved until you press Save.</Notice>}
          <TextArea
            label="Points"
            rows={12}
            maxLength={MAX_CHARS}
            value={points}
            placeholder="Press Generate, or write your own points. One per line."
            onChange={(e) => setPoints(e.target.value)}
            help={`Put {{my_business}} in an AI prompt to use these points. ${points.length} of ${MAX_CHARS} characters.`}
          />
          <div className="flex gap-2">
            <Button busy={save.isPending} disabled={!dirty} onClick={() => save.mutate()}>
              Save
            </Button>
            {isDraft && (
              <Button variant="secondary" onClick={() => setIsDraft(false)}>
                Discard
              </Button>
            )}
          </div>
        </section>
      </div>
    </Page>
  );
}

function Added({ sources, loading }: { sources: S["PersonalizationSource"][] | undefined; loading: boolean }) {
  const refresh = useRefresh();
  const del = useMutation({
    mutationFn: (id: string) => http.delEmpty("/v1/personalization/sources/{id}", { path: { id } }),
    onSuccess: () => refresh(),
    onError: (e) => toast.error(messageOf(e)),
  });
  if (loading) return <Skeleton rows={1} />;
  if (!sources?.length) return null;
  return (
    <ul className="flex flex-wrap gap-2" aria-label="Added">
      {sources.map((s) => (
        <li key={s.id} className="flex items-center gap-1">
          <Pill tone="info">{s.kind === "cv" ? "file" : (s.origin ?? s.tag)}</Pill>
          <button type="button" aria-label={`Remove ${s.tag}`} className="rounded p-1 hover:bg-surface-2" onClick={() => del.mutate(s.id)}>
            <Trash2 size={12} />
          </button>
        </li>
      ))}
    </ul>
  );
}

function UrlCard() {
  const refresh = useRefresh();
  const [url, setUrl] = useState("");
  const add = useMutation({
    mutationFn: () => http.post<S["PersonalizationSource"][]>("/v1/personalization/sources", { body: { kind: "url", url } }),
    onSuccess: async () => {
      await refresh();
      setUrl("");
      toast.success("Link read.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  return (
    <Card>
      <h3 className="mb-2 flex items-center gap-2 text-sm font-medium">
        <Globe size={14} /> Business link
      </h3>
      <div className="flex items-end gap-2">
        <div className="flex-1">
          <Field label="Address" placeholder="https://yourbusiness.com" value={url} onChange={(e) => setUrl(e.target.value)} help="We read the page and up to 4 pages like About or Services." />
        </div>
        <Button busy={add.isPending} disabled={!url.trim()} onClick={() => add.mutate()}>
          Add
        </Button>
      </div>
    </Card>
  );
}

function FileCard() {
  const refresh = useRefresh();
  const up = useMutation({
    mutationFn: (file: File) => {
      const form = new FormData();
      form.append("file", file);
      return upload<S["PersonalizationSource"]>("/v1/personalization/sources/cv", form);
    },
    onSuccess: async () => {
      await refresh();
      toast.success("File read. We keep only the text.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  return (
    <Card>
      <h3 className="mb-2 flex items-center gap-2 text-sm font-medium">
        <FileUp size={14} /> File
      </h3>
      <input
        type="file"
        aria-label="File"
        accept=".pdf,.docx,.txt"
        className="text-sm"
        disabled={up.isPending}
        onChange={(e) => {
          const f = e.target.files?.[0];
          if (f) up.mutate(f);
          e.target.value = "";
        }}
      />
      <p className="mt-1 text-xs text-muted">PDF, DOCX or TXT, up to 5 MB. For example a company profile or a price list. A new file replaces the old one.</p>
    </Card>
  );
}
