import { describe, expect, it } from "vitest";
import {
  aiKeys,
  parse,
  renderPreview,
  required,
  TemplateError,
  variables,
  withFallback,
} from "./template";

describe("template", () => {
  it("reads names and fallbacks, ignoring spaces", () => {
    expect(variables("Hi {{ first_name | there }}, {{company_name}}")).toEqual([
      { name: "first_name", hasFallback: true, raw: "{{ first_name | there }}" },
      { name: "company_name", hasFallback: false, raw: "{{company_name}}" },
    ]);
  });
  it("finds ai slots, and they are not variables", () => {
    const src = "{{ai:opener}} {{ai:close|Bye}} {{x}}";
    expect(aiKeys(src)).toEqual(["opener", "close"]);
    expect(variables(src).map((v) => v.name)).toEqual(["x"]);
  });
  it("refuses a bad name, an open brace and a bad ai key", () => {
    expect(() => parse("{{1abc}}")).toThrow(TemplateError);
    expect(() => parse("Hi {{name")).toThrow(TemplateError);
    expect(() => parse("{{ai:bad key}}")).toThrow(TemplateError);
    expect(() => parse("{{a:b}}")).toThrow(TemplateError);
  });
  it("lists required names: no fallback, not a builtin", () => {
    expect(required("{{a}} {{a}} {{b|x}} {{email}} {{sender_name}}")).toEqual(["a"]);
  });
  it("sets and clears a fallback", () => {
    expect(withFallback("Hi {{first_name}}", "{{first_name}}", "there")).toBe(
      "Hi {{first_name|there}}",
    );
    expect(withFallback("Hi {{first_name|there}}", "{{first_name|there}}", "")).toBe(
      "Hi {{first_name}}",
    );
  });
  it("renders a preview with fallbacks and visible gaps", () => {
    expect(renderPreview("Hi {{n|there}} {{c}} {{ai:o}}", { c: "Acme" })).toBe(
      "Hi there Acme [AI: o]",
    );
    expect(renderPreview("{{ai:o|x}}", { "ai:o": "Real" })).toBe("Real");
  });
});
