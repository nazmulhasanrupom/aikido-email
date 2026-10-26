//! The spec of every node type (build map section 7). 15 triggers, 25 curated AIKIDO nodes and the
//! generic `aikido.api`, 1 AI node, 2 web nodes, 8 flow nodes (with `flow.execute_workflow`) and 9 data nodes (with `code.js`).
//!
//! The param names of the AIKIDO nodes follow the API routes they call. CHECK them against the real
//! request bodies when phase 3 makes these nodes run.
use super::registry::{Category as C, NodeSpec};
use super::schema_kit::*;
use serde_json::{Value, json};

pub fn all_specs() -> Vec<NodeSpec> {
    let mut v = Vec::new();
    v.extend(triggers());
    v.extend(aikido());
    v.push(ai_agent());
    v.extend(web());
    v.extend(flow());
    v.extend(data());
    v
}

// ---------- shared pieces ----------

fn lead_example() -> Value {
    json!({
        "id": "0192f3a0-0000-7000-8000-000000000001", "email": "ana@acme.com", "first_name": "Ana", "last_name": "Lee",
        "company_name": "Acme", "job_title": "CEO", "website": "https://acme.com", "linkedin_url": null,
        "custom_fields": {}, "tags": [], "contact_state": "new", "interest": null
    })
}

fn campaign_example() -> Value {
    json!({ "id": "0192f3a0-0000-7000-8000-000000000002", "name": "Q4 SaaS founders", "status": "running", "content_mode": "ai" })
}

fn event_example() -> Value {
    json!({ "id": "42", "type": "campaign_added", "at": "2026-10-25T09:00:00Z" })
}

fn campaign_id() -> Value {
    string().desc("The campaign id. Use a workflow variable of kind `campaign`.").source("campaigns")
}
fn lead_id() -> Value {
    string().desc("The lead id.")
}
fn list_id() -> Value {
    string().desc("The list id.").source("lists")
}
fn account_id() -> Value {
    string().desc("The email account id.").source("accounts")
}

fn conditions_schema() -> Value {
    array(
        obj()
            .req("left", any().desc("A value or an expression."))
            .req(
                "op",
                one_of(&[
                    "equals", "not_equals", "contains", "not_contains", "starts_with", "ends_with", "gt", "gte", "lt", "lte", "is_empty",
                    "is_not_empty", "is_true", "is_false", "matches",
                ]),
            )
            .opt("right", any().desc("Not used by is_empty, is_not_empty, is_true, is_false."))
            .build(),
    )
    .widget("rules")
}

fn rules_params() -> Value {
    obj()
        .opt("conditions", conditions_schema())
        .opt("combine", one_of(&["and", "or"]).default_value(json!("and")))
        .opt("expression", string().desc("Instead of conditions: one expression that gives true or false."))
        .build()
}

// ---------- triggers (7.1) ----------

fn catch_up() -> Value {
    one_of(&["once", "skip"]).desc("After the server was down: `once` (default) makes one run for the missed time, `skip` makes none. Never a burst.").default_value(json!("once"))
}

fn triggers() -> Vec<NodeSpec> {
    let own = || boolean().desc("Do not start this workflow for changes it made itself.").default_value(json!(true));
    let t = |id, label, desc| NodeSpec::new(id, label, C::Trigger, desc);
    vec![
        t("trigger.manual", "Manual trigger", "Starts when you press Test workflow, or when `POST /v1/automations/workflows/{id}/run` is called.")
            .example(json!({ "started_by": "0192f3a0-0000-7000-8000-000000000009", "input": {} })),
        t("trigger.schedule", "Schedule", "Starts every N minutes, hours or days, or at a time of day.")
            .schema(
                put(
                    obj()
                        .req("mode", one_of(&["interval", "daily"]))
                        .opt("every", integer(1, 100000).desc("Interval mode: how many units."))
                        .opt("unit", one_of(&["minutes", "hours", "days"]))
                        .opt("time", json!({ "type": "string", "pattern": r"^([01]\d|2[0-3]):[0-5]\d$|\$\{[{%]" }).desc("Daily mode: HH:MM in the workflow time zone."))
                        .opt("days", array(one_of(&["mon", "tue", "wed", "thu", "fri", "sat", "sun"])).desc("Daily mode: default every day."))
                        .opt("catch_up", catch_up())
                        .build(),
                    "allOf",
                    json!([
                        { "if": { "properties": { "mode": { "const": "interval" } }, "required": ["mode"] }, "then": { "required": ["every", "unit"] } },
                        { "if": { "properties": { "mode": { "const": "daily" } }, "required": ["mode"] }, "then": { "required": ["time"] } }
                    ]),
                ),
            )
            .example(json!({ "fired_at": "2026-10-25T09:00:00Z", "scheduled_for": "2026-10-25T09:00:00Z", "timezone": "UTC" }))
            .notes("The shortest interval is 1 minute."),
        t("trigger.cron", "Cron", "Starts by a cron expression with 5 fields: minute, hour, day of month, month, day of week.")
            .schema(
                obj()
                    .req("expression", json!({ "type": "string", "pattern": r"^\s*\S+(\s+\S+){4}\s*$|\$\{[{%]" }).desc("For example `0 9 * * 1-5`."))
                    .opt("timezone", string().desc("IANA name. Default: the workflow time zone."))
                    .opt("catch_up", catch_up())
                    .build(),
            )
            .example(json!({ "fired_at": "2026-10-25T09:00:00Z", "scheduled_for": "2026-10-25T09:00:00Z", "timezone": "UTC" })),
        t("trigger.webhook", "Webhook", "Starts when someone calls the webhook URL. The production URL works only while the workflow is active.")
            .schema(
                obj()
                    .opt("path", json!({ "type": "string", "pattern": r"^[a-zA-Z0-9_-]{4,64}$|\$\{[{%]" }).desc("Default: 16 random characters."))
                    .opt("method", one_of(&["POST", "GET", "PUT", "ANY"]).default_value(json!("POST")))
                    .opt("auth", one_of(&["none", "header"]).default_value(json!("none")))
                    .opt("header_name", string().desc("With auth `header`: the header that holds the secret."))
                    .opt("secret", string().desc("With auth `header`: the NAME of the secret.").source("secrets"))
                    .opt("response", one_of(&["immediately", "when_done"]).default_value(json!("immediately")))
                    .build(),
            )
            .example(json!({ "method": "POST", "path": "abc123", "query": {}, "headers": {}, "body": {}, "received_at": "2026-10-25T09:00:00Z" })),
        t("trigger.lead_created", "Lead created", "Starts when a new lead is made.")
            .schema(
                obj()
                    .opt("source", array(one_of(&["api", "csv", "enrollment", "auto_database", "manual"])).desc("Only these sources."))
                    .opt("list_id", list_id())
                    .opt("ignore_own_changes", own())
                    .build(),
            )
            .example(json!({ "event": event_example(), "lead": lead_example() })),
        t("trigger.lead_added_to_campaign", "Lead added to campaign", "Starts when a lead is added to a campaign. Empty `campaign_id` means any campaign.")
            .schema(obj().opt("campaign_id", campaign_id()).opt("ignore_own_changes", own()).build())
            .example(json!({ "event": event_example(), "lead": lead_example(), "campaign": campaign_example() }))
            .notes("With a campaign in AI mode, the feeder waits for the Primary email of the lead, so a workflow can write it first."),
        t("trigger.lead_added_to_list", "Lead added to list", "Starts when a lead is added to a list.")
            .schema(obj().opt("list_id", list_id()).opt("ignore_own_changes", own()).build())
            .example(json!({ "event": event_example(), "lead": lead_example(), "list": { "id": "0192f3a0-0000-7000-8000-000000000003", "name": "Founders" } })),
        t("trigger.lead_updated", "Lead updated", "Starts when a lead changes. With `fields`, only when one of those fields changed.")
            .schema(obj().opt("fields", string_list().desc("Field names, for example `job_title`.")).opt("ignore_own_changes", own()).build())
            .example(json!({ "event": event_example(), "lead": lead_example(), "changed": ["job_title"] })),
        t("trigger.lead_interest_changed", "Lead interest changed", "Starts when the interest of a lead changes.")
            .schema(obj().opt("to", string_list().desc("Only these new values.")).opt("ignore_own_changes", own()).build())
            .example(json!({ "event": event_example(), "lead": lead_example(), "from": null, "to": "interested" })),
        t("trigger.reply_received", "Reply received", "Starts when a reply arrives. Reply Check labels it first.")
            .schema(
                obj()
                    .opt("labels", string_list().desc("Reply Check labels. Default `[\"reply\"]`.").default_value(json!(["reply"])))
                    .opt("campaign_id", campaign_id())
                    .build(),
            )
            .example(json!({
                "event": event_example(),
                "message": { "id": "0192f3a0-0000-7000-8000-000000000004", "label": "reply", "subject": "Re: Hello", "text": "Sounds good.", "from_email": "ana@acme.com",
                    "account_id": "0192f3a0-0000-7000-8000-000000000005", "lead_id": "0192f3a0-0000-7000-8000-000000000001", "campaign_id": null,
                    "message_id_header": "<abc@acme.com>", "references_ids": [], "provider_thread_id": null, "received_at": "2026-10-25T09:00:00Z" },
                "lead": lead_example(), "campaign": null
            })),
        t("trigger.email_sent", "Email sent", "Starts when a campaign email is sent.")
            .schema(obj().opt("campaign_id", campaign_id()).opt("step_number", integer(1, 10)).build())
            .example(json!({
                "event": event_example(),
                "job": { "id": "0192f3a0-0000-7000-8000-000000000006", "to_email": "ana@acme.com", "subject": "Hello", "step_number": 1, "sent_at": "2026-10-25T09:00:00Z", "account_id": "0192f3a0-0000-7000-8000-000000000005" },
                "lead": lead_example(), "campaign": campaign_example()
            })),
        t("trigger.lead_bounced", "Lead bounced", "Starts when an email to a lead bounces.")
            .schema(obj().opt("kinds", array(one_of(&["hard", "soft"])).default_value(json!(["hard"]))).opt("ignore_own_changes", own()).build())
            .example(json!({ "event": event_example(), "lead": lead_example(), "bounce": { "kind": "hard", "at": "2026-10-25T09:00:00Z" } })),
        t("trigger.lead_unsubscribed", "Lead unsubscribed", "Starts when a lead unsubscribes or complains.")
            .schema(obj().opt("ignore_own_changes", own()).build())
            .example(json!({ "event": event_example(), "lead": lead_example(), "reason": "unsubscribed" })),
        t("trigger.campaign_status_changed", "Campaign status changed", "Starts when a campaign changes status.")
            .schema(obj().opt("campaign_id", campaign_id()).opt("to", string_list().desc("Only these new statuses.")).build())
            .example(json!({ "event": event_example(), "campaign": campaign_example(), "from": "draft", "to": "running" }))
            .notes("Reads the table `campaign_events`, which every status change writes (launch, pause, resume, archive, completed)."),
        t("trigger.import_finished", "Import finished", "Starts when a lead CSV import ends.")
            .schema(obj().opt("status", string_list().desc("Default `[\"done\"]`.").default_value(json!(["done"]))).build())
            .example(json!({
                "event": event_example(),
                "import": { "id": "0192f3a0-0000-7000-8000-000000000007", "status": "done", "created": 10, "updated": 2, "skipped": 0, "invalid": 1, "list_id": null, "campaign_id": null }
            })),
    ]
}

// ---------- AIKIDO nodes (7.2) ----------

fn a(id: &'static str, label: &'static str, desc: &'static str) -> NodeSpec {
    NodeSpec::new(id, label, C::Aikido, desc)
}

fn lead_fields_schema() -> Value {
    obj()
        .opt("email", string())
        .opt("first_name", string())
        .opt("last_name", string())
        .opt("company_name", string())
        .opt("job_title", string())
        .opt("phone", string())
        .opt("website", string())
        .opt("linkedin_url", string())
        .opt("location", string())
        .opt("timezone", string())
        .opt("industry", string())
        .opt("personalization_note", string())
        .opt("trigger_event", string())
        .opt("notes", string())
        .opt("tags", string_list())
        .opt("custom_fields", object().desc("Values of custom fields by key."))
        .build()
}

fn aikido() -> Vec<NodeSpec> {
    let max_results = || integer(1, 10000).desc("Stop after this many rows. Default 100.").default_value(json!(100));
    let lead_filter = || {
        obj()
            .opt("q", string().desc("Text search in email, names and company."))
            .opt("list_id", list_id())
            .opt("campaign_id", campaign_id())
            .opt("tag", string())
            .opt("contact_state", string())
            .opt("interest", string())
            .opt("verification", string())
            .opt("suppressed", boolean())
            .opt("created_from", string().desc("ISO time."))
            .opt("created_to", string().desc("ISO time."))
            .build()
    };
    vec![
        a("aikido.lead.get", "Get lead", "Reads one lead.")
            .schema(obj().req("lead_id", lead_id()).build())
            .example(lead_example())
            .open(),
        a("aikido.lead.context", "Lead context", "Reads a lead with its fields, timeline events, messages and opens.")
            .schema(obj().req("lead_id", lead_id()).build())
            .example(json!({ "lead": lead_example(), "fields": {}, "events": [], "messages": [], "opens": [] })),
        a("aikido.lead.find", "Find leads", "Finds leads by filters. Gives one item per lead.")
            .schema(obj().opt("filter", lead_filter()).opt("max_results", max_results()).build())
            .example(lead_example())
            .open(),
        a("aikido.lead.create", "Create lead", "Makes a new lead. Fails when the email exists. Use Upsert to update an existing lead.")
            .schema(lead_fields_schema_with_required_email())
            .side_effect()
            .example(lead_example())
            .open(),
        a("aikido.lead.upsert", "Upsert lead", "Makes a lead or updates the one with the same email.")
            .schema(
                obj()
                    .req("lead", lead_fields_schema())
                    .opt("dedupe_mode", one_of(&["skip", "fill_empty", "overwrite"]).desc("What to do when the email exists. Default `fill_empty`."))
                    .opt("list_id", list_id())
                    .opt("campaign_id", campaign_id())
                    .opt("tags", string_list())
                    .build(),
            )
            .side_effect()
            .example(json!({ "status": "created", "lead_id": "0192f3a0-0000-7000-8000-000000000001", "error": null })),
        a("aikido.lead.update", "Update lead", "Changes fields of a lead. `custom_fields` merges into the old values.")
            .schema(obj().req("lead_id", lead_id()).req("fields", lead_fields_schema()).build())
            .side_effect()
            .example(lead_example())
            .open(),
        a("aikido.lead.add_note", "Add note to lead", "Adds a note to the timeline of a lead.")
            .schema(obj().req("lead_id", lead_id()).req("text", text(5000).widget("textarea")).build())
            .side_effect()
            .example(json!({ "id": "42", "kind": "note", "detail": { "text": "Researched the site." } })),
        a("aikido.lead.action", "Lead action", "Changes many leads at once: lists, tags, interest, verify. Delete is not offered.")
            .schema(
                obj()
                    .req("action", one_of(&["add_to_list", "remove_from_list", "add_tags", "remove_tags", "set_interest", "verify"]))
                    .opt("lead_ids", string_list().desc("Lead ids. Or use `filter`."))
                    .opt("filter", lead_filter())
                    .opt("list_id", list_id())
                    .opt("tags", string_list())
                    .opt("interest", string())
                    .build(),
            )
            .side_effect()
            .example(json!({ "matched": 3, "changed": 3 })),
        a("aikido.lead.events", "Lead events", "Reads the timeline of a lead. Gives one item per event.")
            .schema(obj().req("lead_id", lead_id()).build())
            .example(event_example())
            .open(),
        a("aikido.list.create", "Create list", "Makes a lead list.")
            .schema(obj().req("name", text(200)).opt("description", text(2000)).build())
            .side_effect()
            .example(json!({ "id": "0192f3a0-0000-7000-8000-000000000003", "name": "Founders" }))
            .open(),
        a("aikido.campaign.get", "Get campaign", "Reads one campaign.")
            .schema(obj().req("campaign_id", campaign_id()).build())
            .example(campaign_example())
            .open(),
        a("aikido.campaign.leads", "Campaign leads", "Reads the leads of a campaign with their state. Gives one item per row.")
            .schema(obj().req("campaign_id", campaign_id()).opt("state", string().desc("Only this lead state, for example `pending`.")).opt("max_results", max_results()).build())
            .example(json!({ "lead": lead_example(), "state": "pending" }))
            .open(),
        a("aikido.campaign.add_leads", "Add leads to campaign", "Adds leads to a campaign. They wait as pending until the feeder enrolls them.")
            .schema(
                obj()
                    .req("campaign_id", campaign_id())
                    .opt("lead_ids", string_list().desc("Lead ids. Or use `filter`."))
                    .opt("filter", lead_filter())
                    .build(),
            )
            .side_effect()
            .example(json!({ "matched": 1, "added": 1, "already_in_campaign": 0 })),
        a("aikido.campaign.save_lead_email", "Save lead email", "Writes the email of one lead for one step of a campaign. In AI mode, the lead waits for its Primary email, so save step 1 first.")
            .schema(
                obj()
                    .req("campaign_id", campaign_id())
                    .req("lead_id", lead_id())
                    .req("step", integer(1, 10))
                    .opt("subject", text(500).desc("Step 1 only."))
                    .opt("body_text", text(100000).widget("textarea"))
                    .opt("body_html", text(200000).widget("textarea"))
                    .build(),
            )
            .side_effect()
            .example(json!({ "step": 1, "subject": "Hello", "body_text": "Hi Ana", "status": "ready", "edited_by_hand": true }))
            .open(),
        a("aikido.campaign.get_lead_email", "Get lead email", "Reads the saved email of one lead for one step.")
            .schema(obj().req("campaign_id", campaign_id()).req("lead_id", lead_id()).req("step", integer(1, 10)).build())
            .example(json!({ "step": 1, "subject": "Hello", "body_text": "Hi Ana", "status": "ready" }))
            .open(),
        a("aikido.campaign.status", "Change campaign status", "Launches, pauses or resumes a campaign.")
            .schema(obj().req("campaign_id", campaign_id()).req("action", one_of(&["launch", "pause", "resume"])).build())
            .side_effect()
            .admin()
            .example(campaign_example())
            .open(),
        a("aikido.campaign.stats", "Campaign stats", "Reads the numbers of a campaign.")
            .schema(obj().req("campaign_id", campaign_id()).build())
            .example(json!({ "sent": 10, "replied": 1, "bounced": 0 }))
            .open(),
        a("aikido.email.send", "Send email", "Queues one email. It follows every sending rule of the engine (window, gap, cap, suppressions).")
            .schema(
                obj()
                    .req("account_id", account_id())
                    .req("to_email", string())
                    .opt("to_name", string())
                    .req("subject", text(500))
                    .opt("body_text", text(100000).widget("textarea"))
                    .opt("body_html", text(200000).widget("textarea"))
                    .opt("lead_id", lead_id())
                    .opt("campaign_id", campaign_id())
                    .opt("send_after", string().desc("ISO time."))
                    .opt("priority", integer(0, 1000))
                    .opt("in_reply_to", string())
                    .opt("references", string_list())
                    .opt("provider_thread_id", string())
                    .opt("idempotency_key", string().desc("Default: `wf:{run_id}:{node_id}:{item_index}`."))
                    .build(),
            )
            .side_effect()
            .example(json!({ "status": "queued", "job": { "id": "0192f3a0-0000-7000-8000-000000000006" }, "error": null }))
            .open(),
        a("aikido.email.reply", "Reply to a message", "Queues a reply in the same thread. Fill it from a `trigger.reply_received` message.")
            .schema(
                obj()
                    .req("message", any().desc("The message object, for example `${{ trigger.message }}`."))
                    .req("subject", text(500))
                    .opt("body_text", text(100000).widget("textarea"))
                    .opt("body_html", text(200000).widget("textarea"))
                    .build(),
            )
            .side_effect()
            .example(json!({ "status": "queued", "job": { "id": "0192f3a0-0000-7000-8000-000000000006" }, "error": null }))
            .open(),
        a("aikido.reply.list", "List replies", "Reads stored inbound messages. Gives one item per message.")
            .schema(
                obj()
                    .opt("label", string())
                    .opt("account_id", account_id())
                    .opt("lead_id", lead_id())
                    .opt("campaign_id", campaign_id())
                    .opt("since", string().desc("ISO time."))
                    .opt("handled", boolean())
                    .opt("max_results", max_results())
                    .build(),
            )
            .example(json!({ "id": "0192f3a0-0000-7000-8000-000000000004", "label": "reply", "subject": "Re: Hello", "from_email": "ana@acme.com" }))
            .open(),
        a("aikido.reply.set_label", "Set reply label", "Changes the label of a stored inbound message.")
            .schema(obj().req("message_id", string()).req("label", one_of(&["reply", "auto_reply", "unsubscribe", "bounce_hard", "bounce_soft", "bounce_block", "complaint", "delayed", "unknown"])).build())
            .side_effect()
            .example(json!({ "id": "0192f3a0-0000-7000-8000-000000000004", "label": "reply" }))
            .open(),
        a("aikido.suppression.add", "Add to suppression list", "Blocks an address or a domain from receiving email.")
            .schema(obj().req("value", string().desc("An email address or a domain.")).opt("reason", string()).build())
            .side_effect()
            .example(json!({ "value": "ana@acme.com", "reason": "manual" }))
            .open(),
        a("aikido.enrollment.stop", "Stop enrollment", "Stops the follow-up steps for one enrollment.")
            .schema(obj().req("enrollment_id", string()).build())
            .side_effect()
            .example(json!({ "id": "0192f3a0-0000-7000-8000-000000000008", "status": "stopped" }))
            .open(),
        a("aikido.generator.campaign_test", "Test campaign email", "Writes a test email with the prompts of the campaign. Saves nothing. Costs AI tokens.")
            .schema(obj().req("campaign_id", campaign_id()).req("lead_id", lead_id()).opt("step", integer(1, 10).desc("1 is the Primary email. Default 1.")).build())
            .example(json!({ "mode": "ai", "subject": "Hello", "body": "Hi Ana", "tokens_in": 0, "tokens_out": 0 }))
            .open(),
        a("aikido.verify.reoon", "Verify with Reoon", "Starts a Reoon email check for some leads. Shown only when the Reoon app is installed.")
            .schema(obj().req("lead_ids", string_list()).build())
            .side_effect()
            .example(json!({ "run": { "id": "0192f3a0-0000-7000-8000-00000000000a" }, "estimated_credits": 1 }))
            .open(),
        a("aikido.api", "AIKIDO API (any endpoint)", "Calls any route of the AIKIDO EMAIL API by its operation id. Use a curated node when one exists.")
            .schema(
                obj()
                    .req("operation", string().desc("The OpenAPI `operationId`, or `METHOD /path`."))
                    .opt("path", object().desc("Path parameters."))
                    .opt("query", object())
                    .opt("body", any().desc("An object, or an expression that gives one."))
                    .opt("split", string().desc("Path into the answer (like `leads`): one item per element."))
                    .build(),
            )
            .side_effect()
            .admin()
            .example(json!({}))
            .open()
            .notes("Not offered: /v1/keys, /v1/auth, /v1/users, /v1/dev, /v1/settings/environment, /v1/automations, OAuth routes, and lead delete."),
    ]
}

fn lead_fields_schema_with_required_email() -> Value {
    let mut s = lead_fields_schema();
    s["required"] = json!(["email"]);
    s
}

// ---------- AI (7.3) ----------

fn ai_agent() -> NodeSpec {
    NodeSpec::new("ai.agent", "AI Agent", C::Ai, "Asks an AI model to write text or a JSON object from a prompt. Uses the AI providers set in Settings.")
        .schema(
            obj()
                .opt("provider", string().desc("`default_draft` (strong), `default_slot` (cheap), or a provider name.").default_value(json!("default_draft")).source("providers"))
                .opt("system", text(20000).widget("textarea"))
                .req("prompt", text(50000).widget("textarea"))
                .opt(
                    "output",
                    obj()
                        .opt("format", one_of(&["text", "json"]).default_value(json!("text")))
                        .opt(
                            "fields",
                            array(
                                obj()
                                    .req("key", string())
                                    .opt("type", one_of(&["string", "number", "boolean", "array", "object"]).default_value(json!("string")))
                                    .opt("description", string())
                                    .opt("required", boolean().default_value(json!(true)))
                                    .build(),
                            )
                            .max_items(30)
                            .widget("fields"),
                        )
                        .build(),
                )
                .opt("temperature", json!({ "type": "number", "minimum": 0, "maximum": 2 }))
                .opt("max_tokens", integer(50, 8000).default_value(json!(1000)))
                .opt("timeout_secs", integer(5, 600).default_value(json!(120)))
                .opt("strip_dashes", boolean().desc("Replace em and en dashes with commas.").default_value(json!(true)))
                .opt(
                    "tools",
                    array(
                        obj()
                            .req("name", string().desc("The name the AI uses to call the tool: a to z, 0 to 9 and `_`, starting with a letter."))
                            .req("description", text(500).desc("What the tool does and when to use it. The AI reads this."))
                            .req("node", string().desc("The node type that runs: an `aikido.*` node (not `aikido.api`, not `aikido.campaign.status`), `http.request` or `web.fetch_page`."))
                            .opt("params", object().desc("Fixed params of that node. They can use `${{ }}` and always win over what the AI sends. Put the ids and limits here."))
                            .opt(
                                "args",
                                array(
                                    obj()
                                        .req("key", string())
                                        .opt("type", one_of(&["string", "number", "boolean", "array", "object"]).default_value(json!("string")))
                                        .opt("description", string())
                                        .opt("required", boolean().default_value(json!(true)))
                                        .build(),
                                )
                                .max_items(20)
                                .widget("fields")
                                .desc("The params the AI fills in (name, type, meaning). The AI's values become params of the node."),
                            )
                            .build(),
                    )
                    .max_items(10)
                    .desc("Nodes the AI may call while it works. Empty: a single answer, no tools."),
                )
                .opt("max_tool_calls", integer(1, 20).desc("The most tool calls in one run of the node. More: the AI is told to answer now.").default_value(json!(5)))
                .build(),
        )
        .example(json!({ "text": "Hi Ana, ..." }))
        .open()
        .notes("With `output.format` json, the output has the declared keys (`nodes.write_email.json.subject`). With text, it has `text`. With `tools`, the AI may call the listed nodes before it answers; what each tool call did is in the run log, not in the output. Tool arguments from the AI are plain data: a `${{` in them is refused. In a test run, a tool node that changes data is dry unless Real writes is on (the AI is told what it would have done). Lead text in a prompt can try to steer the AI: keep tools small, lock ids in `params`, and never give a tool that sends or deletes unless you want it.")
}

// ---------- web (7.4) ----------

fn web() -> Vec<NodeSpec> {
    let w = |id, label, desc| NodeSpec::new(id, label, C::Web, desc);
    vec![
        w("http.request", "HTTP Request", "Calls any web API. Secrets go in `auth`, never in the file.")
            .schema(
                obj()
                    .opt("method", one_of(&["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"]).default_value(json!("GET")))
                    .req("url", string().desc("http or https."))
                    .opt("query", string_map())
                    .opt("headers", string_map().desc("`Host` and `Content-Length` are refused."))
                    .opt("body_type", one_of(&["none", "json", "form", "text"]))
                    .opt("body", any())
                    .opt(
                        "auth",
                        obj()
                            .req("type", one_of(&["none", "bearer", "basic", "header"]))
                            .opt("secret", string().desc("The NAME of a secret.").source("secrets"))
                            .opt("user", string())
                            .opt("header_name", string())
                            .build(),
                    )
                    .opt("timeout_secs", integer(1, 120).default_value(json!(30)))
                    .opt("follow_redirects", boolean().default_value(json!(true)))
                    .opt("response_format", one_of(&["auto", "json", "text"]).default_value(json!("auto")))
                    .opt("fail_on_status", boolean().desc("Status 400 or more fails the node.").default_value(json!(true)))
                    .opt("split", string().desc("Path into the JSON body, like `data.results`: one item per element."))
                    .opt(
                        "pagination",
                        obj()
                            .req("mode", one_of(&["next_url", "page", "offset", "cursor"]).desc("`next_url`: the answer holds the address of the next page. `page`: a page number goes up by 1. `offset`: an offset goes up by `step`. `cursor`: the answer holds a cursor for the next call."))
                            .opt("next", string().desc("Where the next address (`next_url`) or the next cursor (`cursor`) is in the answer, as a path into `{ status, headers, body, url }`. Default `body.next` (`next_url`) or `body.next_cursor` (`cursor`)."))
                            .opt("param", string().desc("The name of the page, offset or cursor param (`page`, `offset`, `cursor`). Needed for the modes `page`, `offset` and `cursor`."))
                            .opt("location", one_of(&["query", "body"]).desc("Where `param` goes: the query string (default) or the top level of a JSON object body.").default_value(json!("query")))
                            .opt("start", integer(0, 1_000_000_000).desc("The first page number (default 1) or offset (default 0). The first request uses it."))
                            .opt("step", integer(1, 1_000_000).desc("`offset` only: how much the offset goes up. Default: the number of items of the last page (needs `split`)."))
                            .opt("stop_when", string().desc("An expression. True after a page: no more pages. It reads `page.number`, `page.status`, `page.body`, `page.headers`, `page.items` (the `split` list) and `page.count`, like `${{ page.count == 0 }}`."))
                            .opt("max_pages", integer(1, 100).desc("The most pages one run reads. Default 10. Reaching it is not an error; the log says so.").default_value(json!(10)))
                            .opt("delay_ms", integer(0, 10_000).desc("Wait between two pages.").default_value(json!(0)))
                            .build()
                            .desc("Reads several pages with one node. Not set: one request."),
                    )
                    .build(),
            )
            .example(json!({ "status": 200, "headers": { "content-type": "application/json" }, "body": {}, "url": "https://api.example.com/search" }))
            .open()
            .notes("Add `batch` on the node to slow down for an API with a rate limit. Never scrape a site that forbids it. With `pagination`, the node makes up to `max_pages` requests (they count in the 2000 calls of a run). With `split` the output is one item per element of all pages together; without it, one item per page (with `page` as the page number). A next address on another host is refused (the auth header would leak). A page that fails fails the node; use `max_pages` and `stop_when` to keep it short."),
        w("web.fetch_page", "Fetch web page", "Reads a public web page and gives its text. Respects robots.txt. Does not log in.")
            .schema(
                obj()
                    .req("url", string().desc("A bare domain gets `https://`."))
                    .opt("max_chars", integer(500, 50000).default_value(json!(8000)))
                    .opt("extract", one_of(&["text", "markdown", "html"]).default_value(json!("text")))
                    .opt("include_links", boolean().default_value(json!(false)))
                    .opt("respect_robots", boolean().default_value(json!(true)))
                    .build(),
            )
            .example(json!({ "url": "acme.com", "final_url": "https://acme.com/", "status": 200, "title": "Acme", "description": "We make things.", "text": "Acme ...", "truncated": false }))
            .notes("Use `on_error: continue` and `| default('none')` for sites that may fail. LinkedIn is blocked by default."),
    ]
}

// ---------- flow (7.5) ----------

fn flow() -> Vec<NodeSpec> {
    let f = |id, label, desc| NodeSpec::new(id, label, C::Flow, desc);
    let case_ports: Vec<&'static str> = vec![
        "case_1", "case_2", "case_3", "case_4", "case_5", "case_6", "case_7", "case_8", "case_9", "case_10", "case_11", "case_12", "case_13",
        "case_14", "case_15", "case_16", "case_17", "case_18", "case_19", "case_20", "default",
    ];
    vec![
        f("flow.if", "If", "Sends each item to the `true` or the `false` port.")
            .schema(rules_params())
            .ports(&["true", "false"])
            .open(),
        f("flow.switch", "Switch", "Sends each item to the first case that matches (`case_1`, `case_2`, ...), else to `default`.")
            .schema(
                obj()
                    .req("cases", array(obj().opt("conditions", conditions_schema()).opt("combine", one_of(&["and", "or"])).opt("expression", string()).build()).max_items(20))
                    .opt("first_match_only", boolean().default_value(json!(true)))
                    .build(),
            )
            .ports(&case_ports)
            .open(),
        f("flow.filter", "Filter", "Keeps the items that match and drops the rest.").schema(rules_params()).open(),
        f("flow.merge", "Merge", "Waits for all its inputs, then joins them.")
            .schema(obj().opt("mode", one_of(&["append", "combine_by_position", "wait_all"]).default_value(json!("append"))).build())
            .all_items()
            .open()
            .notes("Prefer one straight chain. Every node can read any upstream node by id."),
        f("flow.wait", "Wait", "Waits for a time. Under 60 seconds it waits in the task. Longer, the run is saved and resumes later.")
            .schema(
                obj()
                    .req("mode", one_of(&["duration", "until"]))
                    .opt("amount", number())
                    .opt("unit", one_of(&["seconds", "minutes", "hours", "days"]))
                    .opt("until", string().desc("An ISO time or an expression that gives one."))
                    .build(),
            )
            .open(),
        f("flow.stop", "Stop", "Ends the run.")
            .schema(obj().opt("message", string()).opt("error", boolean().desc("`true`: the run ends in error. `false`: it ends in success.").default_value(json!(true))).build())
            .ports(&[]),
        f("flow.noop", "No operation", "Does nothing. Useful as a join point.").open(),
        f("flow.execute_workflow", "Execute workflow", "Runs another workflow and gives back its result. A production run uses the published version, a test run the draft.")
            .schema(
                obj()
                    .req("workflow_id", string().desc("The id of the workflow to run.").source("workflows"))
                    .opt("input", one_of(&["each", "all"]).desc("`each` (default): one sub-run per input item; its trigger item is that item. `all`: one sub-run; its trigger item is `{ \"items\": [all input items] }`.").default_value(json!("each")))
                    .opt("trigger", string().desc("The id of the trigger node to start from. Default: the manual trigger, else the first trigger."))
                    .opt("return_node", string().desc("The id of the node in the sub-workflow whose items come back. Default: the last node that made items."))
                    .build(),
            )
            .all_items()
            .open()
            .notes("The sub-workflow runs inside this node: same user, same Cancel, no run row of its own (the log of this run has one line with its nodes). It can go 5 levels deep, and a workflow can not call one that is already running above it (`sub_workflow_loop`). It can wait at most 60 seconds. Its failure fails this node (`sub_workflow_failed`, names the failed node); use `on_error: continue` to go on. In a test run its side-effect nodes are dry and its pins are used. The output is the items of the sub-workflow's last node, all ports together, so end the sub-workflow with a Set fields node when you want a fixed shape."),
    ]
}

// ---------- data (7.6) ----------

fn data() -> Vec<NodeSpec> {
    let d = |id, label, desc| NodeSpec::new(id, label, C::Data, desc);
    vec![
        d("data.set", "Set fields", "Makes or changes fields on each item. A dot in a key makes a nested object (`company.name`).")
            .schema(
                obj()
                    .req("fields", object().desc("Key to value. A value may be an expression.").widget("keyvalue"))
                    .opt("keep", one_of(&["all", "none"]).desc("`all` adds to the input item. `none` keeps only these fields.").default_value(json!("all")))
                    .build(),
            )
            .open(),
        d("data.split_out", "Split out", "Turns a list inside an item into one item per element.")
            .schema(obj().req("path", string().desc("Path to the list, like `leads`.")).opt("keep_parent", boolean()).build())
            .example(json!({ "value": "x" }))
            .open(),
        d("data.aggregate", "Aggregate", "Turns many items into one.")
            .schema(obj().opt("mode", one_of(&["list", "fields"]).default_value(json!("list"))).opt("fields", string_list().desc("With mode `fields`: which fields to collect.")).build())
            .all_items()
            .example(json!({ "items": [] }))
            .open()
            .notes("After Aggregate, `nodes.x.json` of an earlier node is ambiguous. Use `nodes.x.items`."),
        d("data.limit", "Limit", "Keeps N items.")
            .schema(obj().req("max", integer(0, 10000)).opt("from", one_of(&["start", "end"]).default_value(json!("start"))).build())
            .all_items()
            .open(),
        d("data.dedupe", "Remove duplicates", "Drops items whose key was seen before.")
            .schema(obj().req("key", string().desc("An expression per item, like `${{ json.email | lower }}`.")).build())
            .all_items()
            .open(),
        d("data.sort", "Sort", "Sorts items.")
            .schema(obj().req("by", string().desc("An expression per item.")).opt("order", one_of(&["asc", "desc"]).default_value(json!("asc"))).build())
            .all_items()
            .open(),
        d("data.parse_json", "Parse JSON", "Parses a JSON string in a field into an object.")
            .schema(obj().req("path", string()).opt("into", string()).build())
            .open(),
        d("code.js", "Code (JavaScript)", "Runs JavaScript on the items. The code is the body of a function with `items` (a list of objects) and `vars`; it returns a list of objects.")
            .schema(obj().req("code", text(50000).desc("JavaScript. Variables: `items`, `vars`. End with `return items;` or `return [{ ... }]`.").widget("code")).build())
            .all_items()
            .example(json!({ "name": "Ana", "score": 3 }))
            .open()
            .notes("Use it only when Set fields, Filter, Sort and the `${{ }}` filters can not do the job. The code gets all items at once: `return items.map(i => ({ ...i, domain: i.email.split('@')[1] }));`. Return a list of objects (or one object). The same number of items out as in keeps the link to earlier nodes (item i stays item i); another count cuts it, so read earlier nodes with `nodes.x.items`. Limits: 1 second of running time, 32 MB of memory. There is no network, no files, no `require`/`import`, no timers and no `async`/`await`. A `${{` in the code is not an expression. Errors name the line."),
        d("data.date", "Date", "Simple date work: format, add or diff.")
            .schema(
                obj()
                    .req("value", string())
                    .req("action", one_of(&["format", "add", "diff"]))
                    .opt("format", string().desc("strftime style."))
                    .opt("amount", number())
                    .opt("unit", one_of(&["seconds", "minutes", "hours", "days"]))
                    .opt("other", string().desc("For `diff`."))
                    .opt("into", string())
                    .build(),
            )
            .open(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automations::registry::Registry;

    #[test]
    fn the_catalog_has_the_counts_of_section_7() {
        let by = |c: C| all_specs().iter().filter(|s| s.category == c).count();
        assert_eq!(by(C::Trigger), 15);
        assert_eq!(by(C::Aikido), 26, "25 curated nodes and aikido.api");
        assert_eq!(by(C::Ai), 1);
        assert_eq!(by(C::Web), 2);
        assert_eq!(by(C::Flow), 8);
        assert_eq!(by(C::Data), 9, "8 data nodes and code.js");
    }

    #[test]
    fn every_node_has_a_valid_schema_a_description_and_an_example() {
        let reg = Registry::global();
        for s in reg.specs() {
            assert!(s.description.len() > 10, "{} needs a description", s.type_id);
            assert!(!s.label.is_empty());
            let prefixes: &[&str] = if s.category == C::Web {
                &["http.", "web."]
            } else if s.category == C::Data {
                &["data.", "code."]
            } else {
                &[s.category.as_str()]
            };
            assert!(prefixes.iter().any(|p| s.type_id.starts_with(&format!("{}{}", p, if p.ends_with('.') { "" } else { "." }))), "{} has the wrong prefix", s.type_id);
            assert_eq!(s.params_schema["type"], "object", "{}", s.type_id);
            // the clean schema must itself be a valid schema
            jsonschema::validator_for(&s.params_schema).unwrap_or_else(|e| panic!("{}: {e}", s.type_id));
            // an example is valid JSON and an object (the item), checked by round trip
            let text = serde_json::to_string(&s.output_example).unwrap();
            let back: Value = serde_json::from_str(&text).unwrap();
            assert!(back.is_object(), "{}: the output example is an object", s.type_id);
            if s.is_trigger() {
                assert!(!s.output_example.as_object().unwrap().is_empty(), "{}: a trigger needs an example item", s.type_id);
            }
            if !s.is_trigger() && s.type_id != "flow.stop" {
                assert!(!s.ports.is_empty(), "{} has no output port", s.type_id);
            }
        }
    }

    #[test]
    fn side_effect_and_admin_flags() {
        let reg = Registry::global();
        for id in ["aikido.lead.create", "aikido.lead.update", "aikido.campaign.add_leads", "aikido.campaign.save_lead_email", "aikido.email.send"] {
            assert!(reg.spec(id).unwrap().side_effect, "{id} changes data");
        }
        for id in ["aikido.lead.get", "aikido.lead.find", "http.request", "web.fetch_page", "ai.agent"] {
            assert!(!reg.spec(id).unwrap().side_effect, "{id} is not dry in tests");
        }
        assert!(reg.spec("aikido.campaign.status").unwrap().admin_only);
        assert!(reg.spec("aikido.api").unwrap().admin_only);
        assert!(!reg.spec("aikido.lead.get").unwrap().admin_only);
    }

    #[test]
    fn modes() {
        let reg = Registry::global();
        for id in ["flow.merge", "data.aggregate", "data.limit", "data.dedupe", "data.sort"] {
            assert_eq!(reg.spec(id).unwrap().mode, crate::automations::registry::ExecMode::AllItems, "{id}");
        }
        assert_eq!(reg.spec("flow.if").unwrap().ports, vec!["true", "false"]);
    }
}
