import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import {
  Button,
  Card,
  Check2,
  CopyButton,
  Field,
  Notice,
  NumField,
  Pill,
  Select,
  Skeleton,
  Table,
  TD,
  Head,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import {
  type DomainView,
  type Mode,
  type ManualStyle,
  type Preview,
  STATUS_TONE,
  STATUS_WORDS,
  type TrackerType,
  type TrackingBody,
  trackingQuery,
} from "./api";

const FONTS = ["Arial", "Helvetica", "Georgia", "Verdana"];
const DEFAULT_MANUAL: ManualStyle = {
  font: "Arial",
  size: 14,
  color: "#222222",
  bold: false,
  italic: false,
};

/** Campaign Settings > Email Tracking. Off by default. */
export function TrackingSection({ campaignId }: { campaignId: string }) {
  const qc = useQueryClient();
  const q = useQuery(trackingQuery(campaignId));
  const s = q.data;
  const [form, setForm] = useState<TrackingBody | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  useEffect(() => {
    if (!s) return;
    setForm({
      enabled: s.enabled,
      tracker_type: s.tracker_type,
      text_value: s.text_value,
      style_mode: s.style_mode,
      manual_style: s.manual_style,
      image_mode: s.image_mode,
      image_source_url: s.image_source_url,
    });
  }, [s]);
  const save = useMutation({
    mutationFn: (body: TrackingBody) =>
      http.put("/v1/campaigns/{id}/tracking", { path: { id: campaignId }, body }),
    onSuccess: async () => {
      toast.success("Tracking saved.");
      await qc.invalidateQueries({ queryKey: ["tracking", "campaign", campaignId] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const previewIt = useMutation({
    mutationFn: (body: Partial<TrackingBody>) =>
      http.post<Preview>("/v1/campaigns/{id}/tracking/preview", {
        path: { id: campaignId },
        body,
      }),
    onSuccess: setPreview,
    onError: (e) => toast.error(messageOf(e)),
  });
  const refresh = useMutation({
    mutationFn: () =>
      http.post<Preview>("/v1/campaigns/{id}/tracking/refresh-image", {
        path: { id: campaignId },
      }),
    onSuccess: (p) => {
      setPreview(p);
      toast.success("Image downloaded again.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  if (!s || !form) return <Skeleton />;
  const set = (patch: Partial<TrackingBody>) => setForm({ ...form, ...patch });
  const pick = (t: TrackerType) => {
    setPreview(null);
    set({ tracker_type: t });
  };
  const manual = form.manual_style ?? DEFAULT_MANUAL;
  return (
    <Card className="flex flex-col gap-4">
      <div>
        <h2 className="font-semibold">Email tracking</h2>
        <p className="text-sm text-muted">
          Count opens without a hidden pixel. The tracker is a normal image served from your own
          domain, so it does not hurt delivery.
        </p>
      </div>
      <Check2
        label="Email tracking"
        help="Off by default. Only emails with an HTML part get a tracker."
        checked={form.enabled}
        onChange={(on) => {
          set({ enabled: on });
          // the switch alone is saved at once; a tracker is saved with the Save button
          if (!on || !form.tracker_type) save.mutate({ ...form, enabled: on });
        }}
      />
      {s.warning && <Notice tone="warning">{s.warning}</Notice>}
      {form.enabled && (
        <>
          <Domains domains={s.domains} campaignId={campaignId} />
          <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
            <TrackerCard
              title="Text to image"
              locked={!s.trackers_unlocked}
              active={form.tracker_type === "text_image"}
              onPick={() => pick("text_image")}
              text="Replace a word or name in your email with an image of the same text. When the image loads, we count an open."
            >
              <Field
                label="Text to replace"
                placeholder="Nazmul Hasan"
                value={form.text_value ?? ""}
                onChange={(e) => set({ text_value: e.target.value })}
                help="The last match in the email is used. That is usually the signature."
              />
              <Select
                label="Style"
                value={form.style_mode}
                onChange={(v) => set({ style_mode: v as Mode })}
                options={[
                  { value: "auto", label: "Auto: copy the style around the text" },
                  { value: "manual", label: "Manual" },
                ]}
                help="Auto reads only what is written in the email HTML. It is close, not always perfect."
              />
              {form.style_mode === "manual" && (
                <div className="grid grid-cols-2 gap-2">
                  <Select
                    label="Font"
                    value={manual.font}
                    onChange={(v) => set({ manual_style: { ...manual, font: v } })}
                    options={FONTS.map((f) => ({ value: f, label: f }))}
                  />
                  <NumField
                    label="Size"
                    suffix="px"
                    min={6}
                    max={72}
                    value={manual.size}
                    onChange={(n) => set({ manual_style: { ...manual, size: n } })}
                  />
                  <Field
                    label="Color"
                    value={manual.color}
                    onChange={(e) => set({ manual_style: { ...manual, color: e.target.value } })}
                  />
                  <div className="flex flex-col justify-end gap-1">
                    <Check2
                      label="Bold"
                      checked={manual.bold}
                      onChange={(v) => set({ manual_style: { ...manual, bold: v } })}
                    />
                    <Check2
                      label="Italic"
                      checked={manual.italic}
                      onChange={(v) => set({ manual_style: { ...manual, italic: v } })}
                    />
                  </div>
                </div>
              )}
              <PreviewBox
                shown={form.tracker_type === "text_image" ? preview : null}
                busy={previewIt.isPending}
                onPreview={() =>
                  previewIt.mutate({
                    tracker_type: "text_image",
                    text_value: form.text_value,
                    style_mode: form.style_mode,
                    manual_style: form.style_mode === "manual" ? manual : null,
                  })
                }
              />
            </TrackerCard>
            <TrackerCard
              title="Full stop"
              locked={!s.trackers_unlocked}
              active={form.tracker_type === "end_mark_image"}
              onPick={() => pick("end_mark_image")}
              text="The last punctuation mark at the very end of the email becomes the tracker. If the email ends with no mark, a full stop is added. It is tiny, so it hardly shows in dark mode. If it does not load, most mail apps show nothing."
            >
              <Select
                label="Style"
                value={form.style_mode}
                onChange={(v) => set({ style_mode: v as Mode })}
                options={[
                  { value: "auto", label: "Auto: copy the style of the last text" },
                  { value: "manual", label: "Manual" },
                ]}
              />
              {form.style_mode === "manual" && (
                <p className="text-xs text-muted">Uses the font, size and color set under Text to image.</p>
              )}
              <PreviewBox
                shown={form.tracker_type === "end_mark_image" ? preview : null}
                busy={previewIt.isPending}
                onPreview={() =>
                  previewIt.mutate({
                    tracker_type: "end_mark_image",
                    style_mode: form.style_mode,
                    manual_style: form.style_mode === "manual" ? manual : null,
                  })
                }
              />
            </TrackerCard>
            <TrackerCard
              title="Hyphen"
              locked={!s.trackers_unlocked}
              active={form.tracker_type === "hyphen_image"}
              onPick={() => pick("hyphen_image")}
              text="A small hyphen (-) image is added at the very end of the email, after the footer. It is tiny, so it hardly shows in dark mode. If it does not load, most mail apps show nothing."
            >
              <Select
                label="Style"
                value={form.style_mode}
                onChange={(v) => set({ style_mode: v as Mode })}
                options={[
                  { value: "auto", label: "Auto: copy the style of the last text" },
                  { value: "manual", label: "Manual" },
                ]}
              />
              {form.style_mode === "manual" && (
                <p className="text-xs text-muted">Uses the font, size and color set under Text to image.</p>
              )}
              <PreviewBox
                shown={form.tracker_type === "hyphen_image" ? preview : null}
                busy={previewIt.isPending}
                onPreview={() =>
                  previewIt.mutate({
                    tracker_type: "hyphen_image",
                    style_mode: form.style_mode,
                    manual_style: form.style_mode === "manual" ? manual : null,
                  })
                }
              />
            </TrackerCard>
            <TrackerCard
              title="Signature image"
              locked={!s.trackers_unlocked}
              active={form.tracker_type === "signature_image"}
              onPick={() => pick("signature_image")}
              text="Use an image that is already in your email. We serve it from your tracking domain and count an open when it loads."
            >
              <Select
                label="Which image"
                value={form.image_mode}
                onChange={(v) => set({ image_mode: v as Mode })}
                options={[
                  { value: "auto", label: "Auto: the first image that is not in a link" },
                  { value: "manual", label: "Manual: I paste the address" },
                ]}
              />
              {form.image_mode === "manual" && (
                <Field
                  label="Image address"
                  placeholder="https://example.com/logo.png"
                  value={form.image_source_url ?? ""}
                  onChange={(e) => set({ image_source_url: e.target.value })}
                />
              )}
              <PreviewBox
                shown={form.tracker_type === "signature_image" ? preview : null}
                busy={previewIt.isPending}
                onPreview={() =>
                  previewIt.mutate({
                    tracker_type: "signature_image",
                    image_mode: form.image_mode,
                    image_source_url: form.image_source_url,
                  })
                }
              />
              <div>
                <Button
                  variant="secondary"
                  className="h-8"
                  disabled={refresh.isPending || s.tracker_type !== "signature_image"}
                  onClick={() => refresh.mutate()}
                >
                  Refresh image
                </Button>
                <p className="mt-1 text-xs text-muted">
                  We keep one copy of the image. If it changes at the source, download it again.
                </p>
              </div>
            </TrackerCard>
          </div>
          {!s.trackers_unlocked && (
            <Notice tone="info">
              The tracker types unlock when every sending domain is green. Emails from a domain
              that is not green are sent without a tracker.
            </Notice>
          )}
          <div>
            <Button disabled={save.isPending} onClick={() => save.mutate(form)}>
              Save tracking
            </Button>
          </div>
        </>
      )}
    </Card>
  );
}

function TrackerCard({
  title,
  text,
  locked,
  active,
  onPick,
  children,
}: {
  title: string;
  text: string;
  locked: boolean;
  active: boolean;
  onPick: () => void;
  children: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "flex flex-col gap-3 rounded-(--radius-control) border p-3",
        active ? "border-accent" : "border-border",
        locked && "opacity-60",
      )}
    >
      <label className="flex items-start gap-2.5">
        <input
          type="radio"
          name="tracker"
          className="mt-1 accent-(--accent)"
          checked={active}
          disabled={locked}
          onChange={onPick}
        />
        <span className="flex flex-col">
          <span className="font-medium">
            {title} {locked && <Pill tone="warning">Locked</Pill>}
          </span>
          <span className="text-xs text-muted">{text}</span>
        </span>
      </label>
      <fieldset disabled={locked} className="flex flex-col gap-3">
        {children}
      </fieldset>
    </div>
  );
}

function PreviewBox({
  shown,
  busy,
  onPreview,
}: {
  shown: Preview | null;
  busy: boolean;
  onPreview: () => void;
}) {
  return (
    <div className="flex flex-col gap-2">
      <div>
        <Button variant="secondary" className="h-8" disabled={busy} onClick={onPreview}>
          Preview
        </Button>
      </div>
      {shown?.image && (
        <div className="rounded-(--radius-control) border border-border bg-white p-3">
          <img
            src={shown.image}
            alt="Preview of the tracker"
            width={shown.width ?? undefined}
            height={shown.height ?? undefined}
            className="max-w-full"
          />
        </div>
      )}
      {shown?.message && <p className="text-xs text-muted">{shown.message}</p>}
      {shown?.text_found === false && !shown.message && (
        <p className="text-xs text-muted">The text is not in the first email yet.</p>
      )}
    </div>
  );
}

function Domains({ domains, campaignId }: { domains: DomainView[]; campaignId: string }) {
  if (domains.length === 0) {
    return (
      <Notice tone="warning">
        Add a sending account to this campaign first. Tracking needs the domain it sends from.
      </Notice>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <h3 className="text-sm font-medium">Tracking domains</h3>
      <p className="text-xs text-muted">
        Each sending domain needs its own tracking subdomain. Add the CNAME record in your DNS
        provider. We check every 5 minutes and get the HTTPS certificate for you.
      </p>
      <HostingNote />
      <Table>
        <Head cols={["Domain", "Tracking host", "Status", ""]} />
        <tbody>
          {domains.map((d) => (
            <DomainRow key={d.id} d={d} campaignId={campaignId} />
          ))}
        </tbody>
      </Table>
    </div>
  );
}

/** Green here means the CNAME was found. The tracking host must also be served over HTTPS by your own hosting. */
function HostingNote() {
  return (
    <div className="flex flex-col gap-2 rounded-(--radius-control) border border-warning/40 bg-warning/10 p-3 text-sm">
      <p>
        <strong>The tracking host must open over HTTPS on your server.</strong> Green here only means the CNAME was found. Add each tracking host (for example{" "}
        <code>media.yourdomain.com</code>) to the place where Aikido runs, with a valid HTTPS certificate. If you skip this, the image does not load and no opens are counted. The reader sees
        the alt text instead. Use a CNAME only, no A record.
      </p>
      <details>
        <summary className="cursor-pointer font-medium">How to do it in Coolify</summary>
        <ol className="mt-2 list-decimal space-y-1 pl-5">
          <li>
            Open the Aikido app, then <strong>Configuration → General</strong>.
          </li>
          <li>
            In <strong>Domains</strong>, add the tracking host after the ones you have, separated by a comma. For example:{" "}
            <code>https://app.example.com,https://media.yourdomain.com</code>
          </li>
          <li>
            Keep the port at <code>8080</code>. Click <strong>Save</strong>, then <strong>Redeploy</strong>. Coolify makes the HTTPS certificate.
          </li>
          <li>
            If the domain is on Cloudflare, set the <code>media</code> record to <strong>DNS only</strong> (grey cloud) until the certificate works.
          </li>
          <li>
            Check: open <code>https://media.yourdomain.com/i/test.png</code>. A blank page with no error means it works.
          </li>
        </ol>
        <p className="mt-2 text-muted">Do this once for each tracking host. Other hosting: serve the host over HTTPS and send it to the Aikido port (8080).</p>
      </details>
    </div>
  );
}

function DomainRow({ d, campaignId }: { d: DomainView; campaignId: string }) {
  const qc = useQueryClient();
  const [name, setName] = useState(d.subdomain);
  useEffect(() => setName(d.subdomain), [d.subdomain]);
  const refetch = () => qc.invalidateQueries({ queryKey: ["tracking", "campaign", campaignId] });
  const check = useMutation({
    mutationFn: () => http.post(`/v1/tracking/domains/${d.id}/check`),
    onSuccess: refetch,
    onError: (e) => toast.error(messageOf(e)),
  });
  const rename = useMutation({
    mutationFn: () => http.put(`/v1/tracking/domains/${d.id}`, { body: { subdomain: name } }),
    onSuccess: refetch,
    onError: (e) => toast.error(messageOf(e)),
  });
  const record = `${d.record.name}`;
  return (
    <>
      <tr className="border-t border-border align-top">
        <td className={TD}>{d.sending_domain}</td>
        <td className={TD}>
          <div className="flex items-center gap-1.5">
            <input
              aria-label={`Subdomain for ${d.sending_domain}`}
              className="h-8 w-28 rounded-(--radius-control) border border-border bg-surface-2 px-2 text-sm"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
            <span className="text-muted">.{d.sending_domain}</span>
            {name !== d.subdomain && (
              <Button
                variant="secondary"
                className="h-8"
                disabled={rename.isPending}
                onClick={() => rename.mutate()}
              >
                Change
              </Button>
            )}
          </div>
        </td>
        <td className={TD}>
          <Pill dot tone={STATUS_TONE[d.status]}>
            {STATUS_WORDS[d.status]}
          </Pill>
        </td>
        <td className={TD}>
          <Button
            variant="secondary"
            className="h-8"
            disabled={check.isPending}
            onClick={() => check.mutate()}
          >
            Check now
          </Button>
        </td>
      </tr>
      {d.status !== "connected" && (
        <tr>
          <td colSpan={4} className="px-3 pb-3">
            <div className="flex flex-col gap-2 rounded-(--radius-control) bg-surface-2 p-3 text-sm">
              <div className="grid grid-cols-[auto_1fr_auto] items-center gap-x-3 gap-y-1">
                <span className="text-muted">Type</span>
                <code>{d.record.type}</code>
                <CopyButton text={d.record.type} />
                <span className="text-muted">Name</span>
                <code>{record}</code>
                <CopyButton text={record} />
                <span className="text-muted">Value</span>
                <code className="break-all">
                  {d.record.value ?? "(the server host is not set: AIKIDO_TRACKING_CNAME_TARGET)"}
                </code>
                {d.record.value ? <CopyButton text={d.record.value} /> : <span />}
              </div>
              {d.not_found && (
                <p className="text-warning">
                  Not found. We stopped checking after 72 hours. Press Check now to start again.
                </p>
              )}
              {d.error_message && <p className="text-xs text-muted">{d.error_message}</p>}
            </div>
          </td>
        </tr>
      )}
    </>
  );
}
