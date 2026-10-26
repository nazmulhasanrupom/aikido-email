import type { Obj, WfNode } from "./types";

/** What the person picked to test a trigger with. It lives in the page, not in the workflow. */
export interface TestInput {
  leadId?: string;
  leadLabel?: string;
  campaignId?: string;
  listId?: string;
  messageId?: string;
  messageLabel?: string;
  /** Manual: the `input` object. Webhook: the body. Written as JSON text. */
  json?: string;
}

export type TestEvent = { lead_id?: string; campaign_id?: string; list_id?: string; message_id?: string };

export interface TestBody {
  trigger_node_id: string;
  trigger_item?: Obj;
  test_event?: TestEvent;
}

const LEAD_TRIGGERS = new Set([
  "trigger.lead_created",
  "trigger.lead_added_to_campaign",
  "trigger.lead_added_to_list",
  "trigger.lead_updated",
  "trigger.lead_interest_changed",
  "trigger.email_sent",
  "trigger.lead_bounced",
  "trigger.lead_unsubscribed",
]);

export const needsLead = (type: string) => LEAD_TRIGGERS.has(type);

/** The part of the test request that belongs to a trigger. A bad JSON text is an `error`. Nothing picked is fine: the example item is used. */
export function testBody(node: WfNode, input: TestInput | undefined): TestBody | { error: string } {
  const i = input ?? {};
  const body: TestBody = { trigger_node_id: node.id };
  // nothing picked: the server uses the pin of the trigger, else the example item of the catalog
  if (needsLead(node.type)) {
    if (!i.leadId) return body;
    const ev: TestEvent = { lead_id: i.leadId };
    if (node.type === "trigger.lead_added_to_campaign" && i.campaignId) ev.campaign_id = i.campaignId;
    if (node.type === "trigger.lead_added_to_list" && i.listId) ev.list_id = i.listId;
    body.test_event = ev;
    return body;
  }
  if (node.type === "trigger.reply_received") {
    if (!i.messageId) return body;
    body.test_event = { message_id: i.messageId };
    return body;
  }
  if (node.type === "trigger.manual" || node.type === "trigger.webhook") {
    const text = (i.json ?? "").trim();
    if (!text) return body;
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      return { error: "The test data is not valid JSON." };
    }
    if (node.type === "trigger.manual") {
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return { error: "The input must be a JSON object." };
      body.trigger_item = { input: parsed as Obj };
    } else {
      const path = String(node.params?.path ?? "");
      body.trigger_item = {
        method: String(node.params?.method ?? "POST") === "ANY" ? "POST" : String(node.params?.method ?? "POST"),
        path,
        query: {},
        headers: {},
        body: parsed,
        received_at: new Date().toISOString(),
      };
    }
  }
  return body;
}
