import { describe, expect, it } from "vitest";
import { ApiProblem } from "@/api/errors";
import {
  type Applied,
  chipOf,
  diffText,
  errorShown,
  fixText,
  flashIds,
  MAX_MESSAGE,
  mergeMessages,
  sendCheck,
  takeWantChat,
  undoWords,
  wantChat,
} from "./copilot";

const diff = {
  added: ["a", "b", "c"],
  removed: ["x"],
  changed: ["m"],
  edges_added: 0,
  edges_removed: 0,
};
const ok = (o: Partial<Extract<Applied, { mode: string }>> = {}): Applied => ({
  workflow_id: "w",
  mode: "replace",
  diff,
  draft_history_id: 4,
  ...o,
});

describe("chips", () => {
  it("says what changed", () => {
    expect(diffText(diff)).toBe("3 nodes added, 1 changed, 1 removed");
    expect(chipOf(ok())).toMatchObject({
      kind: "applied",
      text: "Applied: 3 nodes added, 1 changed, 1 removed",
      canUndo: true,
    });
  });
  it("has no chip for an answer only", () => expect(chipOf(null)).toBeNull());
  it("cannot undo a create or an undone change", () => {
    expect(chipOf(ok({ mode: "create" }))).toMatchObject({ canUndo: false });
    expect(chipOf(ok({ undone: true }))).toMatchObject({ canUndo: false, undone: true });
  });
  it("marks a proposal", () => expect(chipOf(ok({ mode: "proposed" }))?.kind).toBe("proposed"));
  it("counts errors of a failure", () => {
    const c = chipOf({
      error: "x",
      error_code: "invalid_workflow",
      errors: [
        { code: "a", message: "m" },
        { code: "b", message: "n" },
      ],
    });
    expect(c).toMatchObject({ kind: "failed", text: "Could not apply: 2 errors" });
  });
  it("flashes added and changed nodes", () => expect(flashIds(ok())).toEqual(["a", "b", "c", "m"]));
});

describe("try to fix", () => {
  it("lists the errors", () => {
    const t = fixText([
      { code: "bad_param", node_id: "n1", path: "params.url", message: "Missing" },
    ]);
    expect(t).toContain("- bad_param (n1 params.url): Missing");
  });
  it("cuts a long list", () => {
    const many = Array.from({ length: 25 }, () => ({ code: "c", message: "m" }));
    expect(fixText(many)).toContain("and 5 more");
  });
});

describe("error words", () => {
  const p = (status: number, code: string, ra?: number) =>
    new ApiProblem(status, code, "server text", ra);
  it("links the AI settings", () => {
    expect(errorShown(p(409, "ai_not_configured"))).toEqual({
      text: "Set up an AI provider in Settings > AI.",
      link: "ai-settings",
    });
  });
  it("uses Retry-After", () =>
    expect(errorShown(p(429, "rate_limited", 12)).text).toContain("12 seconds"));
  it("maps provider errors", () => {
    for (const c of [
      "ai_provider_unavailable",
      "ai_bad_request",
      "ai_refused",
      "ai_bad_response",
      "daily_budget_reached",
      "copilot_disabled",
      "thread_workflow_mismatch",
      "draft_too_large",
    ])
      expect(errorShown(p(409, c)).text).not.toBe("server text");
  });
  it("falls back to the server text", () =>
    expect(errorShown(p(500, "weird")).text).toBe("server text"));
  it("words undo errors", () =>
    expect(undoWords(p(409, "nothing_to_undo"))).toContain("nothing to undo"));
});

describe("input", () => {
  it("blocks empty and too long", () => {
    expect(sendCheck("  ").ok).toBe(false);
    expect(sendCheck("a".repeat(MAX_MESSAGE)).ok).toBe(true);
    expect(sendCheck("a".repeat(MAX_MESSAGE + 3))).toMatchObject({ ok: false, over: 3 });
  });
  it("merges messages by id", () => {
    const m = (id: number, content = "") => ({
      id,
      role: "user" as const,
      content,
      applied: null,
      has_workflow: false,
      created_at: "",
    });
    expect(mergeMessages([m(2), m(1)], [m(2, "new"), m(3)]).map((x) => x.content)).toEqual([
      "",
      "new",
      "",
    ]);
  });
  it("remembers the open chat once", () => {
    wantChat("w1");
    expect(takeWantChat("w2")).toBe(false);
    expect(takeWantChat("w1")).toBe(true);
    expect(takeWantChat("w1")).toBe(false);
  });
});
