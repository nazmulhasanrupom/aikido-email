import { describe, expect, it } from "vitest";
import {
  addNode,
  connect,
  connectProblem,
  defaultParams,
  diffDefs,
  fragmentFromText,
  fragmentOf,
  insertOnEdge,
  outPorts,
  parentOf,
  pasteFragment,
  publishSignature,
  pruneEdges,
  removeNodes,
  renameNode,
  setPath,
  slugBase,
  uniqueId,
  upstream,
  wholeExpression,
  wouldCycle,
} from "./defs";
import type { NodeSpec, WfDef, WfNode } from "./types";

const spec = (type: string, extra: Partial<NodeSpec> = {}): NodeSpec => ({
  type,
  label: type,
  category: type.startsWith("trigger.") ? "trigger" : "flow",
  description: "",
  params_schema: { type: "object", properties: {} },
  ports: ["main"],
  mode: "per_item",
  side_effect: false,
  admin_only: false,
  output_example: {},
  docs_notes: "",
  ...extra,
});

const def = (): WfDef => ({
  format: "aikido-workflow",
  version: 1,
  name: "T",
  nodes: [
    { id: "start", type: "trigger.manual" },
    { id: "a", type: "data.set", params: { fields: { x: "${{ trigger.input.x }}" } } },
    { id: "b", type: "data.set", params: { fields: { y: "${{ nodes.a.json.x }}" } } },
  ],
  edges: [
    { from: "start", to: "a" },
    { from: "a", to: "b" },
  ],
});

describe("ids", () => {
  it("makes ids from types", () => {
    expect(slugBase("web.fetch_page")).toBe("web_fetch_page");
    expect(slugBase("trigger.lead_created")).toBe("on_lead_created");
    expect(slugBase("aikido.lead.get")).toBe("lead_get");
  });
  it("keeps ids free and valid", () => {
    expect(uniqueId("a", ["a", "a_2"])).toBe("a_3");
    expect(uniqueId("json", [])).toBe("json_2");
    expect(uniqueId("1x", [])).toBe("n_1x");
  });
});

describe("graph edits", () => {
  it("adds a node after another one and connects it", () => {
    const r = addNode(def(), spec("flow.noop"), { after: { id: "b", port: "main" } });
    expect(r.id).toBe("flow_noop");
    expect(r.def.edges.at(-1)).toEqual({ from: "b", to: "flow_noop" });
  });
  it("does not connect a trigger as a target, a loop, or twice", () => {
    const d = def();
    expect(connectProblem(d, "b", "main", "start")).toBe("to_trigger");
    expect(connectProblem(d, "b", "main", "a")).toBe("cycle");
    expect(connectProblem(d, "a", "main", "b")).toBe("duplicate");
    expect(connectProblem(d, "a", "main", "a")).toBe("self");
    expect(connect(d, "b", "main", "a")).toBe(d);
    expect(wouldCycle(d, "a", "start")).toBe(true);
  });
  it("writes the port only when it is not main", () => {
    const d = connect(def(), "start", "true", "b");
    expect(d.edges.at(-1)).toEqual({ from: "start", port: "true", to: "b" });
  });
  it("removes a node with its edges and its pin", () => {
    const d = { ...def(), pins: { a: [{ x: 1 }], b: [{ y: 2 }] } };
    const r = removeNodes(d, ["a"]);
    expect(r.nodes.map((n) => n.id)).toEqual(["start", "b"]);
    expect(r.edges).toEqual([]);
    expect(r.pins).toEqual({ b: [{ y: 2 }] });
  });
  it("puts a node in the middle of an edge", () => {
    const added = addNode(def(), spec("flow.noop"));
    const r = insertOnEdge(added.def, { from: "a", to: "b" }, added.id);
    expect(r.edges).toContainEqual({ from: "a", to: "flow_noop" });
    expect(r.edges).toContainEqual({ from: "flow_noop", to: "b" });
    expect(r.edges).not.toContainEqual({ from: "a", to: "b" });
  });
  it("lists upstream nodes nearest first", () => {
    expect(upstream(def(), "b")).toEqual(["a", "start"]);
    expect(parentOf(def(), "b")?.from).toBe("a");
  });
});

describe("ports", () => {
  it("gives Switch one port per case and a default", () => {
    const n = { id: "s", type: "flow.switch", params: { cases: [{}, {}, {}] } };
    expect(outPorts(n, spec("flow.switch"))).toEqual(["case_1", "case_2", "case_3", "default"]);
  });
  it("adds the error port", () => {
    const n = { id: "s", type: "flow.noop", on_error: "error_port" as const };
    expect(outPorts(n, spec("flow.noop"))).toEqual(["main", "error"]);
  });
});

describe("new node params", () => {
  it("writes a webhook path so both URLs match", () => {
    const p = defaultParams(spec("trigger.webhook"));
    expect(String(p.path)).toMatch(/^[a-z0-9]{16}$/);
  });
  it("takes schema defaults and the first choice of a required list", () => {
    const s = spec("http.request", {
      params_schema: {
        type: "object",
        required: ["method"],
        properties: {
          method: { type: "string", enum: ["GET", "POST"] },
          timeout_secs: { type: "integer", default: 30 },
        },
      },
    });
    expect(defaultParams(s)).toEqual({ method: "GET", timeout_secs: 30 });
  });
});

describe("copy and paste", () => {
  it("copies nodes with the edges between them and follows references", () => {
    const frag = fragmentOf(def(), ["a", "b"]);
    expect(frag.edges).toEqual([{ from: "a", to: "b" }]);
    const r = pasteFragment(def(), frag);
    expect(r.ids).toEqual(["a_2", "b_2"]);
    const b2 = r.def.nodes.find((n) => n.id === "b_2");
    expect(JSON.stringify(b2?.params)).toContain("nodes.a_2.json.x");
    expect(r.def.edges).toContainEqual({ from: "a_2", to: "b_2" });
  });
  it("reads a workflow file or a fenced list from the clipboard", () => {
    const text = `\`\`\`json\n${JSON.stringify(def())}\n\`\`\``;
    expect(fragmentFromText(text)?.nodes).toHaveLength(3);
    expect(fragmentFromText("hello")).toBeNull();
    expect(fragmentFromText('{"nodes": []}')).toBeNull();
  });
});

describe("diff", () => {
  it("tells added, removed and changed nodes, and ignores positions", () => {
    const a = def();
    const b = structuredClone(a);
    b.nodes[0] = { ...(b.nodes[0] as WfNode), position: { x: 5, y: 5 } };
    expect(diffDefs(a, b)).toEqual({ added: [], removed: [], changed: [], edgesAdded: 0, edgesRemoved: 0 });
    const c = removeNodes(structuredClone(a), ["b"]);
    c.nodes[1] = { id: "a", type: "data.set", params: { fields: {} } };
    const d = diffDefs(a, c);
    expect(d.removed).toEqual(["b"]);
    expect(d.changed).toEqual(["a"]);
    expect(d.edgesRemoved).toBe(1);
    expect(diffDefs(null, a).added).toHaveLength(3);
  });
});

describe("small helpers", () => {
  it("finds a whole expression", () => {
    expect(wholeExpression(" ${{ json.a }} ")).toBe("json.a");
    expect(wholeExpression("hi ${{ json.a }}")).toBeNull();
    expect(wholeExpression("${{ a }} and ${{ b }}")).toBeNull();
    expect(wholeExpression(5)).toBeNull();
  });
  it("sets a deep value and removes an empty one", () => {
    expect(setPath({}, ["output", "format"], "json")).toEqual({ output: { format: "json" } });
    expect(setPath({ a: 1, b: 2 }, ["a"], undefined)).toEqual({ b: 2 });
  });
});

describe("rename and prune", () => {
  it("renames a node everywhere", () => {
    const d = { ...def(), pins: { a: [{ x: 1 }] } };
    const r = renameNode(d, "a", "fetch");
    expect(r.nodes.map((n) => n.id)).toEqual(["start", "fetch", "b"]);
    expect(r.edges).toEqual([{ from: "start", to: "fetch" }, { from: "fetch", to: "b" }]);
    expect(JSON.stringify(r.nodes[2]?.params)).toContain("nodes.fetch.json.x");
    expect(Object.keys(r.pins ?? {})).toEqual(["fetch"]);
  });
  it("refuses a bad or taken id", () => {
    const d = def();
    expect(renameNode(d, "a", "b")).toBe(d);
    expect(renameNode(d, "a", "json")).toBe(d);
    expect(renameNode(d, "a", "Bad Id")).toBe(d);
  });
  it("drops edges from a port that is gone", () => {
    const d: WfDef = {
      ...def(),
      nodes: [{ id: "start", type: "trigger.manual" }, { id: "s", type: "flow.switch", params: { cases: [{}] } }, { id: "b", type: "flow.noop" }],
      edges: [{ from: "start", to: "s" }, { from: "s", port: "case_1", to: "b" }, { from: "s", port: "case_2", to: "b" }],
    };
    const r = pruneEdges(d, "s", spec("flow.switch"));
    expect(r.edges).toHaveLength(2);
    expect(pruneEdges(r, "s", spec("flow.switch"))).toBe(r);
  });
  it("ignores positions in the publish signature", () => {
    const a = def();
    const b = structuredClone(a);
    b.nodes[0] = { ...(b.nodes[0] as WfNode), position: { x: 9, y: 9 } };
    expect(publishSignature(a)).toBe(publishSignature(b));
    b.nodes[1] = { id: "a", type: "data.set", params: {} };
    expect(publishSignature(a)).not.toBe(publishSignature(b));
  });
});
