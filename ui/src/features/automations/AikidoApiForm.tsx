import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { cn } from "@/lib/cn";
import { aq } from "./api";
import type { Leveled } from "./ctx";
import { CONTROL, ObjectForm } from "./ParamForm";
import type { ApiOperation, Obj, Schema } from "./types";

/** The form of `aikido.api`: a picker for the operation, then the fields that operation takes. */
export function AikidoApiForm({
  nodeId,
  params,
  onChange,
  issues,
  disabled,
}: {
  nodeId: string;
  params: Obj;
  onChange: (p: Obj) => void;
  issues: Leveled[];
  disabled?: boolean;
}) {
  const ops = useQuery(aq.operations);
  const operation = typeof params.operation === "string" ? params.operation : "";
  const current = ops.data?.find((o) => o.operation === operation);
  const isExpr = operation.includes("${{");

  const schema = useMemo<Schema>(() => {
    const properties: Record<string, Schema> = {};
    if (current && current.path_params.length > 0) {
      properties.path = {
        type: "object",
        description: "Values for the parts of the path.",
        properties: Object.fromEntries(current.path_params.map((p) => [p, { type: "string" } as Schema])),
        required: current.path_params,
      };
    }
    properties.query = {
      type: "object",
      additionalProperties: { type: "string" },
      description: current?.query_params.length ? `Known names: ${current.query_params.join(", ")}.` : "Query parameters.",
    };
    if (!current || current.has_body) properties.body = { description: "An object, or an expression that gives one." };
    properties.split = { type: "string", description: "Path into the answer (like leads): one item per element." };
    return { type: "object", properties };
  }, [current]);

  const rest = Object.fromEntries(Object.entries(params).filter(([k]) => k !== "operation"));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1.5">
        <span className="text-sm font-medium">
          Operation<span className="text-danger"> *</span>
        </span>
        {isExpr ? (
          <input aria-label="Operation" value={operation} disabled={disabled} onChange={(e) => onChange({ ...params, operation: e.target.value })} className={cn(CONTROL, "font-mono text-[13px]")} />
        ) : (
          <OperationPicker
            ops={ops.data ?? []}
            value={operation}
            disabled={disabled}
            onPick={(o) => onChange({ operation: o.operation, ...(operation === o.operation ? rest : {}) })}
          />
        )}
        {current?.summary && <p className="text-xs text-muted">{current.summary}</p>}
        {current && current.method !== "GET" && <p className="text-xs text-muted">In tests this call is dry: nothing is sent unless Real writes is on.</p>}
        {issues
          .filter((i) => i.path === "params.operation")
          .map((i, n) => (
            <p key={n} className="text-xs text-danger">
              {i.message}
            </p>
          ))}
      </div>
      <ObjectForm
        schema={schema}
        value={rest}
        path="params"
        ctx={{ nodeId, issues, ...(disabled ? { disabled } : {}) }}
        onChange={(next) => onChange({ operation, ...next })}
      />
    </div>
  );
}

function OperationPicker({
  ops,
  value,
  onPick,
  disabled,
}: {
  ops: ApiOperation[];
  value: string;
  onPick: (o: ApiOperation) => void;
  disabled?: boolean | undefined;
}) {
  const [text, setText] = useState("");
  const [open, setOpen] = useState(false);
  const hits = useMemo(() => {
    const t = text.trim().toLowerCase();
    const list = t ? ops.filter((o) => `${o.method} ${o.path} ${o.tag} ${o.summary}`.toLowerCase().includes(t)) : ops;
    return list.slice(0, 80);
  }, [ops, text]);
  const groups = new Map<string, ApiOperation[]>();
  for (const o of hits) groups.set(o.tag || "other", [...(groups.get(o.tag || "other") ?? []), o]);
  return (
    <div className="relative">
      <input
        aria-label="Operation"
        value={open ? text : value}
        disabled={disabled}
        placeholder="Search: GET /v1/leads, campaigns, replies…"
        onFocus={() => {
          setText("");
          setOpen(true);
        }}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        onChange={(e) => setText(e.target.value)}
        className={cn(CONTROL, "font-mono text-[13px]")}
      />
      {open && (
        <div className="absolute top-full left-0 z-50 mt-1 max-h-72 w-full overflow-auto rounded-(--radius-menu) border border-border bg-surface p-1 shadow-(--shadow-pop)">
          {hits.length === 0 && <p className="px-2 py-1.5 text-sm text-muted">No operation found.</p>}
          {[...groups].map(([tag, list]) => (
            <div key={tag}>
              <p className="px-2 pt-1.5 pb-0.5 text-[11px] font-semibold tracking-wide text-muted uppercase">{tag}</p>
              {list.map((o) => (
                <button
                  key={o.operation}
                  type="button"
                  onMouseDown={(e) => {
                    e.preventDefault();
                    onPick(o);
                    setOpen(false);
                  }}
                  className="flex w-full items-baseline gap-2 rounded-(--radius-control) px-2 py-1 text-left text-sm hover:bg-surface-2"
                >
                  <span className={cn("w-12 shrink-0 font-mono text-[11px]", o.method === "GET" ? "text-success" : "text-warning")}>{o.method}</span>
                  <span className="truncate font-mono text-[12px]">{o.path}</span>
                </button>
              ))}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
