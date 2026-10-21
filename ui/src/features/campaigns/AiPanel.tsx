import { useQuery } from "@tanstack/react-query";
import { Braces, Sparkles } from "lucide-react";
import { useRef, useState } from "react";
import type { S } from "@/api/http";
import { q } from "@/api/queries";
import { MenuContent, MenuItem, MenuLabel, MenuRoot, MenuSeparator, MenuTrigger } from "@/components/shell/menu";
import { Button, Pill } from "@/components/ui";
import { AI_PRESETS, type PromptPreset } from "@/lib/aiPresets";
import { GenerateDialog, RunProgress, TestDialog } from "./AiTools";
import { stepName } from "./EmailDrawer";

const LEAD_VARS = ["first_name", "last_name", "company_name", "job_title", "website", "location", "phone", "linkedin_url"];
const PROMPT_ONLY = [
  ["brief", "The text in About your offer"],
  ["my_business", "Your business points from Personalize"],
  ["previous_emails", "The saved text of this lead's earlier emails"],
  ["lead_notes", "The last 5 notes about the lead"],
] as const;

/** The tag of a step email: {{primary_email}}, {{follow_up1}}, {{follow_up2}}, ... */
export const stepTag = (n: number) => (n === 1 ? "primary_email" : `follow_up${n - 1}`);

const AREA =
  "w-full rounded-(--radius-control) border border-border bg-surface-2 p-3 text-sm leading-relaxed";

/** AI mode: a system prompt for the campaign, a user prompt for each step. */
export function AiPanel({
  campaign,
  stepIndex,
  stepCount,
  userPrompt,
  system,
  onSystem,
  onUser,
  onPreset,
  aiReady,
}: {
  campaign: S["CampaignDetail"];
  stepIndex: number;
  stepCount: number;
  userPrompt: string;
  system: string;
  onSystem: (v: string) => void;
  onUser: (v: string) => void;
  onPreset: (p: PromptPreset) => void;
  aiReady: boolean;
}) {
  const fields = useQuery(q.fields);
  const sysRef = useRef<HTMLTextAreaElement>(null);
  const userRef = useRef<HTMLTextAreaElement>(null);
  const [focus, setFocus] = useState<"system" | "user">("user");
  const [gen, setGen] = useState(false);
  const [test, setTest] = useState(false);
  const step = stepIndex + 1;

  function insert(text: string) {
    const ref = focus === "system" ? sysRef : userRef;
    const value = focus === "system" ? system : userPrompt;
    const set = focus === "system" ? onSystem : onUser;
    const at = ref.current?.selectionStart ?? value.length;
    set(value.slice(0, at) + text + value.slice(at));
    requestAnimationFrame(() => {
      ref.current?.focus();
      ref.current?.setSelectionRange(at + text.length, at + text.length);
    });
  }
  const privacy = /\{\{\s*(email|phone)\s*[|}]/.test(`${system}\n${userPrompt}`);

  return (
    <section className="flex min-w-0 flex-col gap-3" aria-label="AI prompts">
      <RunProgress campaignId={campaign.id} />
      {!aiReady && (
        <p className="rounded-(--radius-control) border border-warning/40 bg-warning/10 px-3 py-2 text-sm">
          Set up an AI model first (Settings, AI). Without it nothing can be written.
        </p>
      )}
      <label className="flex flex-col gap-1.5 text-sm">
        <span className="font-medium">System prompt (all steps)</span>
        <textarea
          ref={sysRef}
          rows={5}
          value={system}
          maxLength={8000}
          placeholder="You write short cold emails for {{sender_name}}. ..."
          onFocus={() => setFocus("system")}
          onChange={(e) => onSystem(e.target.value)}
          className={AREA}
        />
      </label>
      <label className="flex flex-col gap-1.5 text-sm">
        <span className="font-medium">User prompt: {stepName(step)}</span>
        <textarea
          ref={userRef}
          rows={10}
          value={userPrompt}
          maxLength={8000}
          placeholder="Write to {{first_name}} at {{company_name}}. ..."
          onFocus={() => setFocus("user")}
          onChange={(e) => onUser(e.target.value)}
          className={AREA}
        />
      </label>
      {privacy && (
        <p className="text-sm text-warning" role="note">
          <Pill tone="warning">Privacy</Pill> The prompt sends the email address or phone number to the AI company.
          Remove it if you do not need it.
        </p>
      )}
      <p className="text-xs text-muted">
        The server adds the output rules (JSON, plain text, no placeholders, a word limit). The footer is
        added after the AI text. The AI never writes it.
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <MenuRoot>
          <MenuTrigger className="inline-flex h-9 items-center gap-2 rounded-(--radius-control) border border-border bg-surface px-3 text-sm font-medium hover:bg-surface-2">
            <Braces size={14} /> Variable
          </MenuTrigger>
          <MenuContent align="start" className="max-h-80 overflow-auto">
            <MenuLabel>Lead</MenuLabel>
            {LEAD_VARS.map((v) => (
              <MenuItem key={v} onSelect={() => insert(`{{${v}}}`)}>
                {v}
              </MenuItem>
            ))}
            {(fields.data?.length ?? 0) > 0 && (
              <>
                <MenuSeparator />
                <MenuLabel>Custom fields</MenuLabel>
                {fields.data?.map((f) => (
                  <MenuItem key={f.key} onSelect={() => insert(`{{${f.key}}}`)}>
                    {f.label || f.key}
                  </MenuItem>
                ))}
              </>
            )}
            <MenuSeparator />
            <MenuLabel>Only in prompts</MenuLabel>
            {PROMPT_ONLY.map(([v, help]) => (
              <MenuItem key={v} onSelect={() => insert(`{{${v}}}`)} title={help}>
                {v}
              </MenuItem>
            ))}
            <MenuSeparator />
            <MenuLabel>Saved emails of the lead</MenuLabel>
            {Array.from({ length: stepCount }, (_, i) => stepTag(i + 1)).map((v, i) => (
              <MenuItem key={v} onSelect={() => insert(`{{${v}}}`)} title={`The saved text of the ${stepName(i + 1)}`}>
                {v}
              </MenuItem>
            ))}
            <MenuSeparator />
            <MenuLabel>Sender</MenuLabel>
            <MenuItem onSelect={() => insert("{{sender_name}}")}>sender_name</MenuItem>
            <MenuItem onSelect={() => insert("{{sender_email}}")}>sender_email</MenuItem>
          </MenuContent>
        </MenuRoot>
        <MenuRoot>
          <MenuTrigger className="inline-flex h-9 items-center gap-2 rounded-(--radius-control) border border-border bg-surface px-3 text-sm font-medium hover:bg-surface-2">
            Presets
          </MenuTrigger>
          <MenuContent align="start">
            <MenuLabel>Fills the prompts of all steps</MenuLabel>
            {AI_PRESETS.map((p) => (
              <MenuItem key={p.name} onSelect={() => onPreset(p)} title={p.help}>
                {p.name}
              </MenuItem>
            ))}
          </MenuContent>
        </MenuRoot>
        <Button variant="secondary" disabled={!aiReady || !userPrompt.trim()} onClick={() => setTest(true)}>
          Test on 1 lead
        </Button>
        <Button disabled={!aiReady} onClick={() => setGen(true)}>
          <Sparkles size={14} /> Generate
        </Button>
      </div>
      <GenerateDialog open={gen} onClose={() => setGen(false)} campaign={campaign} />
      <TestDialog open={test} onClose={() => setTest(false)} campaign={campaign} step={step} />
    </section>
  );
}
