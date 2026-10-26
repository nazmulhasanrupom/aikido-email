import { describe, expect, it } from "vitest";
import { testBody } from "./testInput";
import type { WfNode } from "./types";

const node = (type: string, params: Record<string, unknown> = {}): WfNode => ({ id: "t", type, params });

describe("test body of a trigger", () => {
  it("uses the example item when nothing is picked", () => {
    expect(testBody(node("trigger.lead_created"), undefined)).toEqual({ trigger_node_id: "t" });
    expect(testBody(node("trigger.reply_received"), {})).toEqual({ trigger_node_id: "t" });
  });
  it("builds a test event from the picked lead, campaign, list and message", () => {
    expect(testBody(node("trigger.lead_created"), { leadId: "l1", campaignId: "c1" })).toEqual({ trigger_node_id: "t", test_event: { lead_id: "l1" } });
    expect(testBody(node("trigger.lead_added_to_campaign"), { leadId: "l1", campaignId: "c1" })).toEqual({ trigger_node_id: "t", test_event: { lead_id: "l1", campaign_id: "c1" } });
    expect(testBody(node("trigger.lead_added_to_list"), { leadId: "l1", listId: "x" })).toEqual({ trigger_node_id: "t", test_event: { lead_id: "l1", list_id: "x" } });
    expect(testBody(node("trigger.reply_received"), { messageId: "m1" })).toEqual({ trigger_node_id: "t", test_event: { message_id: "m1" } });
  });
  it("wraps the manual input and the webhook body", () => {
    expect(testBody(node("trigger.manual"), { json: '{"a":1}' })).toEqual({ trigger_node_id: "t", trigger_item: { input: { a: 1 } } });
    const w = testBody(node("trigger.webhook", { path: "abc12345", method: "ANY" }), { json: '{"x":2}' });
    expect("trigger_item" in w && w.trigger_item).toMatchObject({ method: "POST", path: "abc12345", body: { x: 2 } });
  });
  it("says when the JSON is bad", () => {
    expect(testBody(node("trigger.manual"), { json: "{" })).toEqual({ error: "The test data is not valid JSON." });
    expect(testBody(node("trigger.manual"), { json: "[1]" })).toEqual({ error: "The input must be a JSON object." });
  });
  it("needs nothing for a schedule", () => {
    expect(testBody(node("trigger.schedule"), undefined)).toEqual({ trigger_node_id: "t" });
  });
});
