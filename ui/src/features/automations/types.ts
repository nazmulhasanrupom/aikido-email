/** The shapes of the automation API (the generated schema does not cover these routes yet). */

export type Json = string | number | boolean | null | Json[] | { [key: string]: Json };
export type Obj = Record<string, unknown>;

export type OnError = "stop" | "continue" | "error_port";

export interface WfNode {
  id: string;
  type: string;
  name?: string;
  params?: Obj;
  on_error?: OnError;
  retry?: { max: number; delay_ms?: number };
  batch?: { size: number; delay_ms?: number };
  disabled?: boolean;
  position?: { x: number; y: number };
  notes?: string;
}

export interface WfEdge {
  from: string;
  port?: string;
  to: string;
}

export type VarKind =
  | "text"
  | "number"
  | "boolean"
  | "campaign"
  | "list"
  | "sequence"
  | "account"
  | "provider"
  | "custom_field"
  | "json";

export interface WfVariable {
  key: string;
  label?: string;
  kind: VarKind;
  value?: unknown;
  required?: boolean;
  description?: string;
}

export interface WfSticky {
  id: string;
  text: string;
  position: { x: number; y: number };
  size?: { width: number; height: number };
}

export interface WfSettings {
  timezone?: string;
  on_error?: OnError;
  max_concurrent_runs?: number;
  max_runs_per_hour?: number;
  run_timeout_secs?: number;
  save_data?: "all" | "errors_only" | "none";
  [key: string]: unknown;
}

export interface WfDef {
  format: "aikido-workflow";
  version: number;
  name: string;
  description?: string;
  variables?: WfVariable[];
  settings?: WfSettings;
  nodes: WfNode[];
  edges: WfEdge[];
  notes?: WfSticky[];
  pins?: Record<string, Obj[]>;
  meta?: Obj;
}

export type Category = "trigger" | "aikido" | "ai" | "web" | "flow" | "data";

export interface NodeSpec {
  type: string;
  label: string;
  category: Category;
  description: string;
  params_schema: Schema;
  ports: string[];
  mode: "per_item" | "all_items";
  side_effect: boolean;
  admin_only: boolean;
  output_example: unknown;
  docs_notes: string;
}

/** The part of JSON Schema that the node catalog uses. */
export interface Schema {
  type?: string;
  description?: string;
  default?: unknown;
  enum?: unknown[];
  properties?: Record<string, Schema>;
  required?: string[];
  items?: Schema;
  additionalProperties?: Schema | boolean;
  minimum?: number;
  maximum?: number;
  maxLength?: number;
  maxItems?: number;
  pattern?: string;
  "x-ui"?: { widget?: string; source?: string };
  [key: string]: unknown;
}

export interface Issue {
  code: string;
  node_id?: string | null;
  path?: string | null;
  message: string;
}

export interface Validation {
  ok: boolean;
  errors: Issue[];
  warnings: Issue[];
}

export interface PublishedInfo {
  version: number;
  note: string;
  at: string;
}

export interface WorkflowRow {
  id: string;
  name: string;
  description: string;
  active: boolean;
  published_version: number | null;
  draft_changed: boolean;
  draft_valid: boolean;
  last_run: { status: string; at: string } | null;
  runs_24h: { success: number; error: number };
  tags: string[];
  archived_at: string | null;
  updated_at: string;
}

export interface WorkflowDetail {
  id: string;
  name: string;
  description: string;
  active: boolean;
  tags: string[];
  archived_at: string | null;
  draft: WfDef;
  draft_hash: string;
  validation: Validation;
  published: PublishedInfo | null;
  run_as_user_id: string | null;
  updated_at: string;
}

export type RunStatus = "queued" | "running" | "waiting" | "success" | "error" | "cancelled";
export type NodeStatus = "running" | "success" | "error" | "skipped" | "dry" | "waiting";

export interface RunError {
  code: string;
  message: string;
  node_id?: string;
  item_index?: number;
  status?: number;
}

export interface RunRow {
  id: string;
  workflow_id: string;
  version: number | null;
  mode: "test" | "production";
  status: RunStatus;
  trigger_node_id: string;
  queued_at: string;
  started_at: string | null;
  finished_at: string | null;
  duration_ms: number | null;
  error_code: string | null;
}

export interface RunNodeRow {
  node_id: string;
  seq: number;
  status: NodeStatus;
  input_count: number;
  output_count: Record<string, number>;
  output: Record<string, Obj[]> | null;
  truncated_items?: Record<string, number>;
  truncated: boolean;
  error: RunError | null;
  started_at: string;
  finished_at: string | null;
}

export interface RunDetail extends RunRow {
  trigger_item: Obj;
  options: Obj;
  error: RunError | null;
  resume_at: string | null;
  cancel_requested: boolean;
  active_ms: number;
  nodes: RunNodeRow[];
}

export interface RunEvent {
  id: number;
  at: string;
  kind: string;
  node_id: string | null;
  detail: Obj;
}

export interface VersionRow {
  version: number;
  note: string;
  published_by: string | null;
  published_at: string;
  is_published: boolean;
}

export interface SecretRow {
  name: string;
  last4: string;
  updated_at: string;
}

export interface ApiOperation {
  operation: string;
  method: string;
  path: string;
  tag: string;
  summary: string;
  path_params: string[];
  query_params: string[];
  has_body: boolean;
}

export interface TriggerInfo {
  node_id: string;
  kind: string;
  node_type: string;
  webhook_url: string | null;
  next_run_at: string | null;
}

export const FINISHED: RunStatus[] = ["success", "error", "cancelled"];
