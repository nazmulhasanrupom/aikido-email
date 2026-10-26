import { queryOptions, useQuery } from "@tanstack/react-query";
import { problemFrom } from "@/api/errors";
import { http } from "@/api/http";
import { q } from "@/api/queries";
import type { ChatMessage, Thread } from "./copilot";
import type { ImportCheckReply, ImportReply, ImportSource, TemplateInfo } from "./transfer";
import type {
  ApiOperation,
  NodeSpec,
  Obj,
  RunDetail,
  RunEvent,
  RunRow,
  SecretRow,
  TriggerInfo,
  Validation,
  VersionRow,
  WfDef,
  WorkflowDetail,
  WorkflowRow,
} from "./types";

const base = "/v1/automations";

export const aq = {
  workflows: (filter: { q?: string; archived?: boolean; active?: boolean } = {}) =>
    queryOptions({
      queryKey: ["automations", "list", filter],
      queryFn: () =>
        http.get<{ workflows: WorkflowRow[]; next_cursor: string | null }>(`${base}/workflows`, {
          query: { q: filter.q, archived: filter.archived, active: filter.active, limit: 200 },
        }),
      refetchInterval: 30_000,
    }),
  workflow: (id: string) =>
    queryOptions({
      queryKey: ["automations", "workflow", id],
      queryFn: () => http.get<WorkflowDetail>(`${base}/workflows/{id}`, { path: { id } }),
    }),
  catalog: queryOptions({
    queryKey: ["automations", "nodes"],
    queryFn: async () => (await http.get<{ nodes: NodeSpec[] }>(`${base}/nodes`)).nodes,
    staleTime: Number.POSITIVE_INFINITY,
  }),
  operations: queryOptions({
    queryKey: ["automations", "operations"],
    queryFn: async () =>
      (await http.get<{ operations: ApiOperation[] }>(`${base}/nodes/aikido-api/operations`))
        .operations,
    staleTime: Number.POSITIVE_INFINITY,
  }),
  secrets: queryOptions({
    queryKey: ["automations", "secrets"],
    queryFn: async () => (await http.get<{ secrets: SecretRow[] }>(`${base}/secrets`)).secrets,
  }),
  settings: queryOptions({
    queryKey: ["automations", "settings"],
    queryFn: () => http.get<Obj>(`${base}/settings`),
  }),
  status: queryOptions({
    queryKey: ["automations", "status"],
    queryFn: () => http.get<EngineStatus>(`${base}/status`),
    refetchInterval: 15_000,
  }),
  versions: (id: string) =>
    queryOptions({
      queryKey: ["automations", "versions", id],
      queryFn: async () =>
        (
          await http.get<{ versions: VersionRow[] }>(`${base}/workflows/{id}/versions`, {
            path: { id },
          })
        ).versions,
    }),
  version: (id: string, version: number) =>
    queryOptions({
      queryKey: ["automations", "versions", id, version],
      queryFn: () =>
        http.get<{ definition: WfDef }>(`${base}/workflows/{id}/versions/{version}`, {
          path: { id, version },
        }),
    }),
  runs: (workflowId: string, f: { mode?: string; status?: string } = {}) =>
    queryOptions({
      queryKey: ["automations", "runs", workflowId, f],
      queryFn: () =>
        http.get<{ runs: RunRow[]; next_cursor: string | null }>(`${base}/runs`, {
          query: { workflow_id: workflowId, mode: f.mode, status: f.status, limit: 100 },
        }),
      refetchInterval: 5_000,
    }),
  templates: queryOptions({
    queryKey: ["automations", "templates"],
    queryFn: async () =>
      (await http.get<{ templates: TemplateInfo[] }>(`${base}/templates`)).templates,
    staleTime: Number.POSITIVE_INFINITY,
  }),
  run: (id: string) =>
    queryOptions({
      queryKey: ["automations", "run", id],
      queryFn: () => http.get<RunDetail>(`${base}/runs/{id}`, { path: { id } }),
    }),
};

export interface EngineStatus {
  paused: boolean;
  worker_enabled: boolean;
  last_tick_at: string | null;
  queue: { status: string; mode: string; count: number }[];
  active_workflows: number;
  active_triggers?: number;
  trigger_problems?: {
    workflow_id: string;
    workflow_name: string;
    node_id: string;
    kind: string;
    last_error: string | null;
    rate_limited_at: string | null;
  }[];
  warnings?: string[];
}

/** The automations feature is on. Until the config is loaded it counts as on, so the menu does not jump. */
export function useAutomationsOn(): boolean {
  const config = useQuery(q.config);
  const f = config.data?.features as { automations?: boolean } | undefined;
  return f?.automations ?? true;
}

/** A text answer (a file). The key order of the text is the server's, so it is never parsed here. */
async function getText(url: string): Promise<{ text: string; disposition: string | null }> {
  const res = await fetch(url, { credentials: "same-origin" });
  if (!res.ok) throw problemFrom(res, await res.json().catch(() => undefined));
  return { text: await res.text(), disposition: res.headers.get("content-disposition") };
}

// ---------- calls ----------

export const call = {
  get: (id: string) => http.get<WorkflowDetail>(`${base}/workflows/{id}`, { path: { id } }),
  create: (body: { name: string; definition?: WfDef }) =>
    http.post<WorkflowDetail>(`${base}/workflows`, { body }),
  patch: (
    id: string,
    body: {
      draft?: WfDef;
      if_hash?: string;
      name?: string;
      description?: string;
      tags?: string[];
      archived?: boolean;
    },
  ) => http.patch<WorkflowDetail>(`${base}/workflows/{id}`, { path: { id }, body }),
  remove: (id: string, hard = false) =>
    http.delEmpty(`${base}/workflows/{id}`, { path: { id }, query: hard ? { hard: true } : {} }),
  duplicate: (id: string) =>
    http.post<WorkflowDetail>(`${base}/workflows/{id}/duplicate`, { path: { id } }),
  exportFile: (id: string, o: { version?: number; includePins?: boolean }) => {
    const qs = new URLSearchParams();
    if (o.version !== undefined) qs.set("version", String(o.version));
    if (o.includePins) qs.set("include_pins", "true");
    return getText(`${base}/workflows/${encodeURIComponent(id)}/export${qs.size ? `?${qs}` : ""}`);
  },
  importWorkflow: (
    source: ImportSource,
    extra: { name?: string; variables?: Record<string, string> } = {},
  ) => http.post<ImportReply>(`${base}/import`, { body: { ...source, ...extra } }),
  importCheck: (source: ImportSource) =>
    http.post<ImportCheckReply>(`${base}/import/check`, { body: source }),
  createFromTemplate: (id: string, body: { name?: string; variables?: Record<string, string> }) =>
    http.post<ImportReply>(`${base}/templates/{id}/create`, { path: { id }, body }),
  workprocess: (install: boolean) =>
    getText(`${base}/workprocess.md${install ? "?install=true" : ""}`),
  validate: (definition: WfDef, forPublish = false) =>
    http.post<Validation>(`${base}/validate`, { body: { definition, for_publish: forPublish } }),
  publish: (id: string, note: string) =>
    http.post<{ version: number; workflow: WorkflowDetail }>(`${base}/workflows/{id}/publish`, {
      path: { id },
      body: { note },
    }),
  activate: (id: string, backfillHours: number) =>
    http.post<{ workflow: WorkflowDetail; triggers: TriggerInfo[] }>(
      `${base}/workflows/{id}/activate`,
      { path: { id }, body: { backfill_hours: backfillHours } },
    ),
  deactivate: (id: string, cancelRuns: boolean) =>
    http.post<WorkflowDetail>(`${base}/workflows/{id}/deactivate`, {
      path: { id },
      body: { cancel_runs: cancelRuns },
    }),
  restoreVersion: (id: string, version: number) =>
    http.post<WorkflowDetail>(`${base}/workflows/{id}/versions/{version}/restore`, {
      path: { id, version },
    }),
  test: (
    id: string,
    body: {
      trigger_node_id?: string;
      trigger_item?: Obj;
      only_node?: string;
      real_writes?: boolean;
      variables?: Obj;
      test_event?: {
        lead_id?: string;
        campaign_id?: string;
        list_id?: string;
        message_id?: string;
      };
    },
  ) => http.post<{ run_id: string }>(`${base}/workflows/{id}/test`, { path: { id }, body }),
  listen: (id: string, nodeId: string) =>
    http.post<{ test_url: string; path: string; until: string }>(`${base}/workflows/{id}/listen`, {
      path: { id },
      body: { node_id: nodeId },
    }),
  runNow: (id: string, input: Obj) =>
    http.post<{ run_id: string }>(`${base}/workflows/{id}/run`, { path: { id }, body: { input } }),
  listRuns: (workflowId: string, mode: string) =>
    http.get<{ runs: RunRow[]; next_cursor: string | null }>(`${base}/runs`, {
      query: { workflow_id: workflowId, mode, limit: 1 },
    }),
  runDetail: (id: string) => http.get<RunDetail>(`${base}/runs/{id}`, { path: { id } }),
  cancel: (id: string) => http.post<unknown>(`${base}/runs/{id}/cancel`, { path: { id } }),
  retry: (id: string) => http.post<{ run_id: string }>(`${base}/runs/{id}/retry`, { path: { id } }),
  events: (id: string, after: number) =>
    http.get<{ status: RunDetail["status"]; events: RunEvent[] }>(`${base}/runs/{id}/events`, {
      path: { id },
      query: { after, limit: 500 },
    }),
  runNode: (id: string, nodeId: string, offset = 0, limit = 100) =>
    http.get<{
      node_id: string;
      seq: number;
      status: string;
      input_count: number;
      output_count: Record<string, number>;
      output: Record<string, Obj[]>;
      truncated: boolean;
      error: Obj | null;
    }>(`${base}/runs/{id}/nodes/{node_id}`, {
      path: { id, node_id: nodeId },
      query: { offset, limit },
    }),
  preview: (body: {
    workflow_id: string;
    node_id: string;
    text: string;
    run_id?: string;
    item_index?: number;
  }) =>
    http.post<{
      value: unknown;
      type: string | null;
      error: { code: string; message: string } | null;
      run_id: string | null;
    }>(`${base}/expression/preview`, { body }),
  cron: (expression: string, timezone: string) =>
    http.get<{ timezone: string; next: string[]; local: string[] }>(`${base}/cron/preview`, {
      query: { expression, timezone, count: 5 },
    }),
  saveSecret: (name: string, value: string) =>
    http.post<unknown>(`${base}/secrets`, { body: { name, value } }),
  deleteSecret: (name: string) => http.delEmpty(`${base}/secrets/{name}`, { path: { name } }),
  saveSettings: (body: Obj) => http.patch<Obj>(`${base}/settings`, { body }),
  pause: () => http.post<EngineStatus>(`${base}/engine/pause`),
  resume: () => http.post<EngineStatus>(`${base}/engine/resume`),
};

export interface SendReply {
  thread: Thread;
  user_message: ChatMessage;
  message: ChatMessage;
  workflow_id: string | null;
  draft_hash: string | null;
}

const cp = `${base}/copilot`;

/** The AI box (map 16). */
export const copilot = {
  threads: (workflowId?: string) =>
    http.get<{ threads: Thread[] }>(`${cp}/threads`, {
      query: workflowId ? { workflow_id: workflowId } : {},
    }),
  createThread: (workflowId?: string) =>
    http.post<Thread>(`${cp}/threads`, { body: workflowId ? { workflow_id: workflowId } : {} }),
  deleteThread: (id: string) => http.delEmpty(`${cp}/threads/{id}`, { path: { id } }),
  messages: (id: string) =>
    http.get<{ thread: Thread; messages: ChatMessage[] }>(`${cp}/threads/{id}/messages`, {
      path: { id },
      query: { limit: 200 },
    }),
  send: (id: string, body: { text: string; workflow_id?: string; apply?: boolean }) =>
    http.post<SendReply>(`${cp}/threads/{id}/messages`, { path: { id }, body }),
  undo: (messageId: number) =>
    http.post<{ message: ChatMessage; workflow: WorkflowDetail }>(`${cp}/messages/{id}/undo`, {
      path: { id: messageId },
    }),
};
