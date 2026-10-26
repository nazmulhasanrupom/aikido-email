import { describe, expect, it } from "vitest";
import { suggest } from "./data";
import { specialKind } from "./ParamForm";
import {
  allowedToolNode,
  codeErrorLine,
  freeToolName,
  indentSelection,
  PAGE_ROOT,
  PAGE_SAMPLE,
  paginationFields,
  paginationProblems,
  pruneForMode,
  returnNodes,
  toolProblems,
  triggerNodes,
} from "./phase9";
import type { Schema, WfDef } from "./types";

describe("code errors", () => {
  it("reads the line from a syntax or runtime message", () => {
    expect(
      codeErrorLine([{ path: "params.code", message: "Syntax error: unexpected token (line 4)" }]),
    ).toBe(4);
    expect(
      codeErrorLine([{ path: "params.code", message: "The code failed: boom (line 3)" }]),
    ).toBe(3);
  });
  it("ignores other fields and messages without a line", () => {
    expect(codeErrorLine([{ path: "params.other", message: "x (line 2)" }])).toBeNull();
    expect(codeErrorLine([{ path: "params.code", message: "The code is too long." }])).toBeNull();
  });
});

describe("tab key", () => {
  it("inserts two spaces at the caret", () => {
    expect(indentSelection("ab", 1, 1, false)).toEqual({ text: "a  b", start: 3, end: 3 });
  });
  it("indents and outdents every selected line", () => {
    const r = indentSelection("a\nb", 0, 3, false);
    expect(r.text).toBe("  a\n  b");
    expect(indentSelection(r.text, 0, r.end, true).text).toBe("a\nb");
  });
  it("outdent on a line without indent changes nothing", () => {
    expect(indentSelection("a", 0, 0, true).text).toBe("a");
  });
});

describe("sub-workflow pickers", () => {
  const def = {
    nodes: [
      { id: "t1", type: "trigger.manual", name: "Start" },
      { id: "n1", type: "data.set" },
      { id: "t2", type: "trigger.webhook" },
    ],
  } as unknown as WfDef;
  it("lists trigger nodes for trigger and the rest for return_node", () => {
    expect(triggerNodes(def).map((n) => n.id)).toEqual(["t1", "t2"]);
    expect(returnNodes(def).map((n) => n.id)).toEqual(["n1"]);
    expect(triggerNodes(def)[0]?.label).toBe("Start");
  });
});

describe("agent tools", () => {
  it("allows aikido nodes, http and fetch, not the admin ones", () => {
    expect(allowedToolNode("aikido.lead.get")).toBe(true);
    expect(allowedToolNode("http.request")).toBe(true);
    expect(allowedToolNode("web.fetch_page")).toBe(true);
    expect(allowedToolNode("aikido.api")).toBe(false);
    expect(allowedToolNode("aikido.campaign.status")).toBe(false);
    expect(allowedToolNode("data.set")).toBe(false);
  });
  it("finds bad and repeated names, descriptions and nodes", () => {
    const p = toolProblems([
      { name: "get_lead", description: "d", node: "aikido.lead.get" },
      { name: "get_lead", description: "d", node: "http.request" },
      { name: "Bad", description: "", node: "data.set" },
    ]);
    expect(p.map((x) => `${x.index}:${x.field}`)).toEqual([
      "1:name",
      "2:name",
      "2:description",
      "2:node",
    ]);
  });
  it("accepts a good tool and gives free names", () => {
    expect(toolProblems([{ name: "a1", description: "x", node: "web.fetch_page" }])).toEqual([]);
    expect(freeToolName([{ name: "tool_1" }, { name: "tool_2" }])).toBe("tool_3");
  });
});

describe("pagination form", () => {
  it("shows the fields of each mode", () => {
    expect(paginationFields("next_url")).toEqual(["next", "stop_when", "max_pages", "delay_ms"]);
    expect(paginationFields("page")).toEqual([
      "param",
      "location",
      "start",
      "stop_when",
      "max_pages",
      "delay_ms",
    ]);
    expect(paginationFields("offset")).toContain("step");
    expect(paginationFields("cursor")).toContain("next");
    expect(paginationFields("cursor")).not.toContain("start");
    expect(paginationFields(undefined)).toEqual(paginationFields("next_url"));
  });
  it("drops the fields of the old mode", () => {
    const r = pruneForMode({ mode: "offset", param: "o", step: 10, max_pages: 3 }, "next_url");
    expect(r).toEqual({ mode: "next_url", max_pages: 3 });
  });
  it("warns like the validator", () => {
    expect(paginationProblems({ mode: "next_url" }, false)).toEqual([]);
    expect(paginationProblems({ mode: "page", param: "page" }, false)).toHaveLength(1);
    expect(
      paginationProblems(
        { mode: "page", param: "page", stop_when: "${{ page.count == 0 }}" },
        false,
      ),
    ).toEqual([]);
    expect(paginationProblems({ mode: "page" }, true)).toEqual([
      "Name the page, offset or cursor param.",
    ]);
    expect(paginationProblems({ mode: "offset", param: "o" }, false)).toHaveLength(2);
    expect(paginationProblems({ mode: "offset", param: "o" }, true)).toEqual([]);
    expect(
      paginationProblems({ mode: "offset", param: "o", stop_when: "${{ true }}" }, false),
    ).toHaveLength(1);
  });
});

describe("page root", () => {
  it("is offered only when passed in, with its keys", () => {
    expect(suggest("pa", {}).items.map((i) => i.text)).not.toContain("page");
    expect(suggest("pa", PAGE_SAMPLE, PAGE_ROOT).items.map((i) => i.text)).toContain("page");
    expect(suggest("page.", PAGE_SAMPLE, PAGE_ROOT).items.map((i) => i.text)).toEqual([
      "number",
      "status",
      "headers",
      "body",
      "url",
      "items",
      "count",
    ]);
  });
});

describe("special params", () => {
  const exec: Schema = {
    type: "object",
    properties: {
      workflow_id: { type: "string", "x-ui": { source: "workflows" } },
      trigger: { type: "string" },
    },
  };
  it("knows the Phase 9 shapes", () => {
    expect(specialKind("trigger", { type: "string" }, exec)).toBe("subnode");
    expect(
      specialKind("trigger", { type: "string" }, { type: "object", properties: {} }),
    ).toBeNull();
    expect(specialKind("code", { type: "string", "x-ui": { widget: "code" } })).toBe("code");
    expect(
      specialKind("pagination", {
        type: "object",
        properties: { mode: { type: "string" }, max_pages: { type: "integer" } },
      }),
    ).toBe("pagination");
    expect(
      specialKind("tools", {
        type: "array",
        items: {
          type: "object",
          properties: { node: { type: "string" }, args: { type: "array" } },
        },
      }),
    ).toBe("tools");
  });
});
