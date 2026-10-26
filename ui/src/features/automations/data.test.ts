import { describe, expect, it } from "vitest";
import { dataOf, pathsOf, sampleContext, suggest } from "./data";
import type { NodeSpec, RunDetail, WfDef } from "./types";

const spec = (type: string, example: unknown): NodeSpec => ({
  type,
  label: type,
  category: "flow",
  description: "",
  params_schema: { type: "object" },
  ports: ["main"],
  mode: "per_item",
  side_effect: false,
  admin_only: false,
  output_example: example,
  docs_notes: "",
});

const def: WfDef = {
  format: "aikido-workflow",
  version: 1,
  name: "T",
  variables: [{ key: "campaign_id", kind: "campaign", value: "c1" }],
  nodes: [
    { id: "start", type: "trigger.manual" },
    { id: "fetch", type: "web.fetch_page" },
    { id: "write", type: "ai.agent" },
  ],
  edges: [
    { from: "start", to: "fetch" },
    { from: "fetch", to: "write" },
  ],
};
const specs = new Map([
  ["trigger.manual", spec("trigger.manual", { started_by: "u", input: {} })],
  ["web.fetch_page", spec("web.fetch_page", { url: "https://x", text: "hello" })],
  ["ai.agent", spec("ai.agent", { text: "" })],
]);

const run = {
  trigger_node_id: "start",
  trigger_item: { input: { name: "Ana" } },
  nodes: [{ node_id: "fetch", seq: 1, status: "success", output: { main: [{ url: "https://acme.com", text: "Anvils" }] }, truncated_items: { main: 3 } }],
} as unknown as RunDetail;

describe("node data", () => {
  it("prefers a pin, then a run, then the example", () => {
    const pinned = { ...def, pins: { fetch: [{ text: "pinned" }] } };
    expect(dataOf(pinned, specs, run, "fetch").source).toBe("pin");
    const fromRun = dataOf(def, specs, run, "fetch");
    expect(fromRun.source).toBe("run");
    expect(fromRun.more).toEqual({ main: 3 });
    expect(dataOf(def, specs, null, "fetch").source).toBe("example");
    expect(dataOf(def, specs, null, "fetch", false).source).toBe("none");
  });
  it("reads the trigger item of the run", () => {
    expect(dataOf(def, specs, run, "start").ports.main).toEqual([{ input: { name: "Ana" } }]);
  });
});

describe("sample context", () => {
  it("has json from the parent, the trigger, upstream nodes and variables", () => {
    const c = sampleContext(def, specs, run, "write") as Record<string, Record<string, unknown>>;
    expect(c.json).toEqual({ url: "https://acme.com", text: "Anvils" });
    expect(c.trigger).toEqual({ input: { name: "Ana" } });
    expect(Object.keys(c.nodes ?? {})).toEqual(["fetch", "start"]);
    expect(c.vars).toEqual({ campaign_id: "c1" });
  });
});

describe("suggestions", () => {
  const sample = sampleContext(def, specs, run, "write");
  it("offers the root names at the start", () => {
    const r = suggest("tr", sample);
    expect(r.items.map((i) => i.text)).toEqual(["trigger"]);
    expect(r.from).toBe(0);
  });
  it("offers the keys after a dot", () => {
    const r = suggest(" json.", sample);
    expect(r.items.map((i) => i.text)).toEqual(["url", "text"]);
    expect(r.from).toBe(" json.".length);
  });
  it("walks into nodes and filters by what was typed", () => {
    expect(suggest("nodes.fetch.j", sample).items.map((i) => i.text)).toEqual(["json"]);
    expect(suggest("nodes.", sample).items.map((i) => i.text)).toContain("fetch");
  });
  it("offers filters after a pipe", () => {
    const r = suggest("json.text | lo", sample);
    expect(r.items.map((i) => i.text)).toEqual(["lower"]);
  });
  it("says nothing for a path that does not exist", () => {
    expect(suggest("json.nope.", sample).items).toEqual([]);
  });
});

describe("paths", () => {
  it("lists dotted paths and walks into the first item of a list", () => {
    expect(pathsOf({ a: { b: 1 }, c: [{ d: 2 }] }).map((p) => p.path)).toEqual(["a", "a.b", "c", "c.0", "c.0.d"]);
  });
});
