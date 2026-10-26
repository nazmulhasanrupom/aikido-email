import { describe, expect, it } from "vitest";
import { ApiProblem } from "@/api/errors";
import {
  actionsFor,
  addWorkflowNodes,
  excerpt,
  fileNameFrom,
  type ImportReport,
  isWholeWorkflow,
  jsonPlace,
  keepReport,
  renameNote,
  setNodeProvider,
  setVariableValue,
  takeReport,
} from "./transfer";
import type { WfDef } from "./types";

const base: WfDef = {
  format: "aikido-workflow",
  version: 1,
  name: "Open",
  nodes: [
    { id: "start", type: "trigger.manual", position: { x: 0, y: 0 } },
    { id: "fetch", type: "web.fetch_page", position: { x: 280, y: 0 } },
  ],
  edges: [{ from: "start", to: "fetch" }],
  variables: [{ key: "campaign_id", kind: "campaign", value: null }],
};

const incoming: WfDef = {
  format: "aikido-workflow",
  version: 1,
  name: "Pasted",
  nodes: [
    { id: "start", type: "trigger.manual", position: { x: 0, y: 0 } },
    {
      id: "fetch",
      type: "web.fetch_page",
      position: { x: 280, y: 0 },
      params: { url: "${{ nodes.start.item.url }}" },
    },
    {
      id: "write",
      type: "data.set",
      position: { x: 560, y: 0 },
      params: { v: "${{ nodes.fetch.item.text }}" },
    },
  ],
  edges: [
    { from: "start", to: "fetch" },
    { from: "fetch", to: "write" },
  ],
  notes: [{ id: "note", text: "hi", position: { x: 10, y: 10 } }],
  pins: { fetch: [{ a: 1 }] },
  variables: [
    { key: "campaign_id", kind: "campaign", value: "other" },
    { key: "list_id", kind: "list", value: null },
  ],
};

describe("add the nodes of a pasted workflow", () => {
  const r = addWorkflowNodes(base, incoming);
  it("renames the nodes whose ids are taken, and only those", () => {
    expect(r.renamed).toEqual([
      ["start", "start_2"],
      ["fetch", "fetch_2"],
    ]);
    expect(r.def.nodes.map((n) => n.id)).toEqual(["start", "fetch", "start_2", "fetch_2", "write"]);
    expect(r.ids).toEqual(["start_2", "fetch_2", "write"]);
  });
  it("follows the rename in expressions and edges", () => {
    const fetch = r.def.nodes.find((n) => n.id === "fetch_2");
    expect(fetch?.params?.url).toBe("${{ nodes.start_2.item.url }}");
    expect(r.def.nodes.find((n) => n.id === "write")?.params?.v).toBe(
      "${{ nodes.fetch_2.item.text }}",
    );
    expect(r.def.edges).toContainEqual({ from: "fetch_2", to: "write" });
    expect(r.def.edges).toContainEqual({ from: "start", to: "fetch" });
    expect(r.def.edges).toHaveLength(3);
  });
  it("puts the new nodes to the right of the old ones", () => {
    const xs = r.def.nodes.slice(2).map((n) => n.position?.x ?? 0);
    expect(Math.min(...xs)).toBe(580);
  });
  it("moves pins and notes, and adds only the missing variables", () => {
    expect(Object.keys(r.def.pins ?? {})).toEqual(["fetch_2"]);
    expect(r.def.notes?.map((n) => n.id)).toEqual(["note"]);
    expect(r.def.variables?.map((v) => v.key)).toEqual(["campaign_id", "list_id"]);
    expect(r.def.variables?.[0]?.value).toBeNull();
  });
  it("keeps ids that are free and does not touch the open draft", () => {
    const again = addWorkflowNodes({ ...base, nodes: [], edges: [] }, incoming);
    expect(again.renamed).toEqual([]);
    expect(base.nodes).toHaveLength(2);
  });
  it("says what was renamed", () => {
    expect(renameNote(r.renamed)).toBe(
      "Renamed start to start_2, fetch to fetch_2 because the ids were taken.",
    );
    expect(renameNote([])).toBe("");
  });
});

describe("paste detection", () => {
  const whole = JSON.stringify(incoming);
  it("sees a whole workflow, wrapped or fenced", () => {
    expect(isWholeWorkflow(whole)).toBe(true);
    expect(isWholeWorkflow(JSON.stringify({ workflow: incoming }))).toBe(true);
    expect(isWholeWorkflow(`\`\`\`json\n${whole}\n\`\`\``)).toBe(true);
  });
  it("leaves the node clipboard to the old paste", () => {
    expect(
      isWholeWorkflow(JSON.stringify({ nodes: incoming.nodes, edges: incoming.edges }, null, 2)),
    ).toBe(false);
  });
  it("ignores other text", () => {
    expect(isWholeWorkflow("hello there, this is plain text")).toBe(false);
    expect(isWholeWorkflow('{"name": "x", "nodes": "no", "format": 1}')).toBe(false);
    expect(isWholeWorkflow("{ broken json of some length here")).toBe(false);
  });
});

describe("report to actions", () => {
  const report: ImportReport = {
    fixed: [],
    warnings: [],
    validation: { ok: true, errors: [], warnings: [] },
    needs_you: [
      {
        kind: "variable",
        key: "campaign_id",
        var_kind: "campaign",
        hint: "Q4",
        message: "Pick the campaign.",
      },
      { kind: "variable", key: "seq", var_kind: "sequence", message: "Pick the sequence." },
      { kind: "secret", name: "SEARCH_API_KEY", message: "Add this secret." },
      { kind: "provider", node_id: "ai", name: "Gone", message: "Pick a provider." },
      { kind: "app", name: "reoon", message: "Install Reoon." },
    ],
  };
  const def: WfDef = {
    ...base,
    nodes: [...base.nodes, { id: "ai", type: "ai.agent", params: { provider: "default_draft" } }],
    variables: [
      { key: "campaign_id", kind: "campaign", value: null },
      { key: "seq", kind: "sequence", value: null },
    ],
  };
  it("gives one action per item with the right kind", () => {
    const a = actionsFor(report, def, new Set());
    expect(a.map((x) => x.kind)).toEqual(["pick", "id", "secret", "provider", "app"]);
    expect(a[0]).toMatchObject({
      key: "campaign_id",
      varKind: "campaign",
      hint: "Q4",
      done: false,
    });
    expect(a.every((x) => !x.done)).toBe(true);
    expect(new Set(a.map((x) => x.id)).size).toBe(5);
  });
  it("marks items done as the draft and the secrets change", () => {
    let d = setVariableValue(def, "campaign_id", "c-1");
    d = setNodeProvider(d, "ai", "Claude-Main");
    const a = actionsFor(report, d, new Set(["SEARCH_API_KEY"]));
    expect(a.map((x) => x.done)).toEqual([true, false, true, true, false]);
  });
  it("adds a variable the draft lacks", () => {
    expect(setVariableValue({ ...base, variables: [] }, "x", "1", "list").variables).toEqual([
      { key: "x", kind: "list", value: "1" },
    ]);
  });
});

describe("errors and files", () => {
  it("reads line and column of invalid_json", () => {
    const e = new ApiProblem(422, "invalid_json", "bad", undefined, { line: 3, column: 7 });
    expect(jsonPlace(e)).toEqual({ line: 3, column: 7 });
    expect(jsonPlace(new ApiProblem(422, "invalid_format", "x"))).toBeNull();
    expect(jsonPlace(new Error("x"))).toBeNull();
  });
  it("shows the broken line with a caret", () => {
    expect(excerpt('{\n  "a": 1,\n  "b" 2\n}', { line: 3, column: 7 })).toEqual({
      line: '  "b" 2',
      caret: "      ^",
    });
    expect(excerpt("{}", { line: 9, column: 1 })).toBeNull();
    const long = excerpt(`${"x".repeat(200)}!`, { line: 1, column: 201 });
    expect(long?.line.startsWith("…")).toBe(true);
    expect(long?.caret.trimEnd().endsWith("^")).toBe(true);
  });
  it("takes the file name from the header, else from the name", () => {
    expect(fileNameFrom('attachment; filename="my-flow.aikido-workflow.json"', "x")).toBe(
      "my-flow.aikido-workflow.json",
    );
    expect(fileNameFrom(null, "Research lead & write")).toBe(
      "research-lead-write.aikido-workflow.json",
    );
    expect(fileNameFrom(null, "???")).toBe("workflow.aikido-workflow.json");
  });
  it("hands a kept report over once", () => {
    const r: ImportReport = {
      fixed: ["a"],
      needs_you: [],
      warnings: [],
      validation: { ok: true, errors: [], warnings: [] },
    };
    keepReport("w1", r);
    expect(takeReport("w1")).toBe(r);
    expect(takeReport("w1")).toBeNull();
  });
});
