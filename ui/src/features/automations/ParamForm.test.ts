import { describe, expect, it } from "vitest";
import { orderedProps, ORDER, parseLoose } from "./ParamForm";
import type { Schema } from "./types";

const schema: Schema = {
  type: "object",
  required: ["prompt"],
  properties: {
    timeout_secs: { type: "integer" },
    output: { type: "object" },
    prompt: { type: "string" },
    mode: { type: "string" },
    alpha: { type: "string" },
  },
};

describe("field order", () => {
  it("puts required fields first, then the ones that choose, then the rest, and advanced ones last", () => {
    expect(orderedProps(schema).map(([k]) => k)).toEqual(["prompt", "mode", "alpha", "output", "timeout_secs"]);
  });
  it("follows an explicit list and keeps unknown fields at the end", () => {
    expect(orderedProps(schema, ["output", "prompt", "nope"]).map(([k]) => k)).toEqual(["output", "prompt", "timeout_secs", "mode", "alpha"]);
  });
  it("has an explicit order for the AI Agent", () => {
    expect(ORDER["ai.agent"]?.slice(0, 3)).toEqual(["provider", "system", "prompt"]);
  });
});

describe("values typed in a table", () => {
  it("keeps numbers, true/false, null and JSON typed, and the rest as text", () => {
    expect(parseLoose("42", false)).toBe(42);
    expect(parseLoose("true", false)).toBe(true);
    expect(parseLoose("null", false)).toBeNull();
    expect(parseLoose('{"a":1}', false)).toEqual({ a: 1 });
    expect(parseLoose("{bad", false)).toBe("{bad");
    expect(parseLoose("hello", false)).toBe("hello");
    expect(parseLoose("${{ json.a }}", false)).toBe("${{ json.a }}");
  });
  it("keeps everything as text when the schema wants text", () => {
    expect(parseLoose("42", true)).toBe("42");
  });
});
