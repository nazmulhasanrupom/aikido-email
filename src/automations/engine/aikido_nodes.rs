//! The AIKIDO nodes (build map 7.2). Each node turns its params into a call to a real route of the app
//! (`internal_call.rs`) and turns the answer into items. There is no key to set up: the call acts as the user
//! of the run.
//!
//! - `plan` builds the call from the params. A dry run (test run) shows the plan and sends nothing.
//! - `Shape` says how the answer becomes items: the body is one item, a list is one item per row (following
//!   `next_cursor`), or the first entry of a bulk answer.
use super::env::NodeEnv;
use super::nodes::get_path;
use crate::automations::internal_call::{CallRequest, CallResponse, deny_reason, fill_path};
use crate::automations::registry::{Item, NodeCtx, NodeError, NodeInput, NodeOutput, NodeSpec, NodeType, closest};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// The most rows a list node reads when `max_results` is not set by the node (lead events).
const DEFAULT_MAX_ROWS: usize = 1000;
const PAGE_SIZE: usize = 500;

/// The node for a spec, or the spec back when it is not an AIKIDO node.
#[allow(clippy::result_large_err)]
pub fn make(spec: NodeSpec) -> Result<Box<dyn NodeType>, NodeSpec> {
    let op = match spec.type_id {
        "aikido.lead.get" => Op::LeadGet,
        "aikido.lead.context" => Op::LeadContext,
        "aikido.lead.find" => Op::LeadFind,
        "aikido.lead.create" => Op::LeadCreate,
        "aikido.lead.upsert" => Op::LeadUpsert,
        "aikido.lead.update" => Op::LeadUpdate,
        "aikido.lead.add_note" => Op::LeadAddNote,
        "aikido.lead.action" => Op::LeadAction,
        "aikido.lead.events" => Op::LeadEvents,
        "aikido.list.create" => Op::ListCreate,
        "aikido.campaign.get" => Op::CampaignGet,
        "aikido.campaign.leads" => Op::CampaignLeads,
        "aikido.campaign.add_leads" => Op::CampaignAddLeads,
        "aikido.campaign.save_lead_email" => Op::SaveLeadEmail,
        "aikido.campaign.get_lead_email" => Op::GetLeadEmail,
        "aikido.campaign.status" => Op::CampaignStatus,
        "aikido.campaign.stats" => Op::CampaignStats,
        "aikido.email.send" => Op::EmailSend,
        "aikido.email.reply" => Op::EmailReply,
        "aikido.reply.list" => Op::ReplyList,
        "aikido.reply.set_label" => Op::ReplySetLabel,
        "aikido.suppression.add" => Op::SuppressionAdd,
        "aikido.enrollment.stop" => Op::EnrollmentStop,
        "aikido.generator.campaign_test" => Op::CampaignTest,
        "aikido.verify.reoon" => Op::VerifyReoon,
        "aikido.api" => Op::Api,
        _ => return Err(spec),
    };
    Ok(Box::new(Aikido { spec, op }))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    LeadGet,
    LeadContext,
    LeadFind,
    LeadCreate,
    LeadUpsert,
    LeadUpdate,
    LeadAddNote,
    LeadAction,
    LeadEvents,
    ListCreate,
    CampaignGet,
    CampaignLeads,
    CampaignAddLeads,
    SaveLeadEmail,
    GetLeadEmail,
    CampaignStatus,
    CampaignStats,
    EmailSend,
    EmailReply,
    ReplyList,
    ReplySetLabel,
    SuppressionAdd,
    EnrollmentStop,
    CampaignTest,
    VerifyReoon,
    Api,
}

/// How the answer of a call becomes items.
#[derive(Debug, Clone, PartialEq)]
enum Shape {
    /// The body is the one item.
    Body,
    /// `body[key]` is a list. One item per element. Pages are read until `max` rows or no `next_cursor`.
    Paged { key: &'static str, max: usize },
    /// `body[key][0]` is the item, cut to the fields in `pick` (bulk routes with one entry).
    First { key: &'static str, pick: &'static [&'static str] },
    /// The `split` path of the `aikido.api` node: one item per element of a list in the body, or the body.
    Split(Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
struct Plan {
    req: CallRequest,
    shape: Shape,
}

struct Aikido {
    spec: NodeSpec,
    op: Op,
}

fn bad(msg: impl Into<String>) -> NodeError {
    NodeError::new("invalid_param_value", msg)
}

// ---------- reading params ----------

struct P<'a> {
    m: &'a Map<String, Value>,
}

impl P<'_> {
    fn get(&self, k: &str) -> Option<&Value> {
        self.m.get(k).filter(|v| !v.is_null())
    }
    fn str(&self, k: &str) -> Result<String, NodeError> {
        match self.get(k) {
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
            Some(Value::Number(n)) => Ok(n.to_string()),
            _ => Err(bad(format!("`{k}` is missing or empty"))),
        }
    }
    fn opt_str(&self, k: &str) -> Option<String> {
        match self.get(k) {
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }
    /// An id (UUID). Checked, so a value can not change the path of the call.
    fn id(&self, k: &str) -> Result<String, NodeError> {
        let s = self.str(k)?;
        Uuid::parse_str(&s).map(|u| u.to_string()).map_err(|_| bad(format!("`{k}` is not a valid id (got `{s}`)")))
    }
    fn opt_id(&self, k: &str) -> Result<Option<String>, NodeError> {
        match self.opt_str(k) {
            None => Ok(None),
            Some(s) => Uuid::parse_str(&s).map(|u| Some(u.to_string())).map_err(|_| bad(format!("`{k}` is not a valid id (got `{s}`)"))),
        }
    }
    fn int(&self, k: &str) -> Result<Option<i64>, NodeError> {
        match self.get(k) {
            None => Ok(None),
            Some(Value::Number(n)) => n.as_i64().map(Some).ok_or_else(|| bad(format!("`{k}` must be a whole number"))),
            Some(Value::String(s)) => s.trim().parse::<i64>().map(Some).map_err(|_| bad(format!("`{k}` must be a whole number (got `{s}`)"))),
            Some(_) => Err(bad(format!("`{k}` must be a whole number"))),
        }
    }
    fn obj(&self, k: &str) -> Result<Option<Map<String, Value>>, NodeError> {
        match self.get(k) {
            None => Ok(None),
            Some(Value::Object(o)) => Ok(Some(o.clone())),
            Some(_) => Err(bad(format!("`{k}` must be an object"))),
        }
    }
    /// The listed keys that have a value, as an object.
    fn pick(&self, keys: &[&str]) -> Map<String, Value> {
        keys.iter().filter_map(|k| self.get(k).map(|v| ((*k).to_string(), v.clone()))).collect()
    }
    fn max_results(&self, default: usize) -> Result<usize, NodeError> {
        Ok(self.int("max_results")?.map(|n| n.clamp(1, 10_000) as usize).unwrap_or(default))
    }
}

const FILTER_KEYS: &[&str] = &["q", "list_id", "campaign_id", "tag", "contact_state", "interest", "verification", "suppressed", "created_from", "created_to"];

fn query_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(a) => Some(a.iter().filter_map(query_text).collect::<Vec<_>>().join(",")),
        other => Some(other.to_string()),
    }
}

fn add_filter_query(mut req: CallRequest, filter: &Map<String, Value>) -> CallRequest {
    for k in FILTER_KEYS {
        if let Some(t) = filter.get(*k).and_then(query_text).filter(|t| !t.is_empty()) {
            req = req.query(k, t);
        }
    }
    req
}

/// A selection of leads for a route: `lead_ids`, or a `filter` object.
fn selection(p: &P) -> Result<Map<String, Value>, NodeError> {
    let mut m = Map::new();
    if let Some(ids) = p.get("lead_ids") {
        if !ids.is_array() {
            return Err(bad("`lead_ids` must be a list of ids"));
        }
        m.insert("lead_ids".into(), ids.clone());
    }
    if let Some(f) = p.obj("filter")? {
        m.insert("filter".into(), Value::Object(f));
    }
    if m.is_empty() {
        return Err(bad("Give `lead_ids` or a `filter`."));
    }
    Ok(m)
}

/// The text of an array or string `references`, as the send route wants it (one string, ids separated by a space).
fn references_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Array(a) => {
            let ids: Vec<&str> = a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).collect();
            (!ids.is_empty()).then(|| ids.join(" "))
        }
        _ => None,
    }
}

impl Aikido {
    /// Build the call for one item. `ctx.params` are evaluated first by the caller.
    fn plan(&self, p: &P, who: &PlanCtx) -> Result<Plan, NodeError> {
        let body = |req: CallRequest, m: Map<String, Value>| req.body(Value::Object(m));
        let plan = |req: CallRequest, shape: Shape| Ok(Plan { req, shape });
        match self.op {
            Op::LeadGet => plan(CallRequest::new("GET", format!("/v1/leads/{}", p.id("lead_id")?)), Shape::Body),
            Op::LeadContext => plan(CallRequest::new("GET", format!("/v1/leads/{}/context", p.id("lead_id")?)), Shape::Body),
            Op::LeadFind => {
                let filter = p.obj("filter")?.unwrap_or_default();
                let req = add_filter_query(CallRequest::new("GET", "/v1/leads"), &filter);
                plan(req, Shape::Paged { key: "leads", max: p.max_results(100)? })
            }
            // the params are the fields of the lead
            Op::LeadCreate => plan(body(CallRequest::new("POST", "/v1/leads"), p.m.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect()), Shape::Body),
            Op::LeadUpsert => {
                let lead = p.obj("lead")?.ok_or_else(|| bad("`lead` is missing"))?;
                let mut b = p.pick(&["dedupe_mode", "list_id", "campaign_id", "tags"]);
                b.insert("leads".into(), json!([lead]));
                plan(body(CallRequest::new("POST", "/v1/leads/bulk"), b), Shape::First { key: "items", pick: &["status", "lead_id", "error"] })
            }
            Op::LeadUpdate => {
                let fields = p.obj("fields")?.ok_or_else(|| bad("`fields` is missing"))?;
                plan(body(CallRequest::new("PATCH", format!("/v1/leads/{}", p.id("lead_id")?)), fields), Shape::Body)
            }
            Op::LeadAddNote => plan(body(CallRequest::new("POST", format!("/v1/leads/{}/notes", p.id("lead_id")?)), p.pick(&["text"])), Shape::Body),
            Op::LeadAction => {
                let action = p.str("action")?;
                if !["add_to_list", "remove_from_list", "add_tags", "remove_tags", "set_interest", "verify"].contains(&action.as_str()) {
                    return Err(bad(format!("The action `{action}` is not offered to workflows. Erasing leads is off.")));
                }
                let mut b = selection(p)?;
                b.insert("action".into(), json!(action));
                b.extend(p.pick(&["list_id", "tags", "interest"]));
                plan(body(CallRequest::new("POST", "/v1/leads/actions"), b), Shape::Body)
            }
            Op::LeadEvents => plan(CallRequest::new("GET", format!("/v1/leads/{}/events", p.id("lead_id")?)), Shape::Paged { key: "events", max: DEFAULT_MAX_ROWS }),
            Op::ListCreate => plan(body(CallRequest::new("POST", "/v1/leads/lists"), p.pick(&["name", "description"])), Shape::Body),
            Op::CampaignGet => plan(CallRequest::new("GET", format!("/v1/campaigns/{}", p.id("campaign_id")?)), Shape::Body),
            Op::CampaignLeads => {
                let mut req = CallRequest::new("GET", format!("/v1/campaigns/{}/leads", p.id("campaign_id")?));
                if let Some(s) = p.opt_str("state") {
                    req = req.query("state", s);
                }
                plan(req, Shape::Paged { key: "leads", max: p.max_results(100)? })
            }
            Op::CampaignAddLeads => plan(body(CallRequest::new("POST", format!("/v1/campaigns/{}/leads", p.id("campaign_id")?)), selection(p)?), Shape::Body),
            Op::SaveLeadEmail => {
                let step = p.int("step")?.filter(|s| (1..=10).contains(s)).ok_or_else(|| bad("`step` must be 1 to 10"))?;
                let path = format!("/v1/campaigns/{}/leads/{}/emails/{step}", p.id("campaign_id")?, p.id("lead_id")?);
                let b = p.pick(&["subject", "body_text", "body_html"]);
                if b.is_empty() {
                    return Err(bad("Give `subject`, `body_text` or `body_html`."));
                }
                plan(body(CallRequest::new("PATCH", path), b), Shape::Body)
            }
            Op::GetLeadEmail => {
                let step = p.int("step")?.filter(|s| (1..=10).contains(s)).ok_or_else(|| bad("`step` must be 1 to 10"))?;
                plan(CallRequest::new("GET", format!("/v1/campaigns/{}/leads/{}/emails/{step}", p.id("campaign_id")?, p.id("lead_id")?)), Shape::Body)
            }
            Op::CampaignStatus => {
                let action = p.str("action")?;
                if !["launch", "pause", "resume"].contains(&action.as_str()) {
                    return Err(bad(format!("`action` must be launch, pause or resume (got `{action}`)")));
                }
                plan(CallRequest::new("POST", format!("/v1/campaigns/{}/{action}", p.id("campaign_id")?)).body(json!({})), Shape::Body)
            }
            Op::CampaignStats => plan(CallRequest::new("GET", format!("/v1/campaigns/{}/stats", p.id("campaign_id")?)), Shape::Body),
            Op::EmailSend => {
                let mut job = p.pick(&["account_id", "to_email", "to_name", "subject", "body_text", "body_html", "lead_id", "campaign_id", "send_after", "priority", "in_reply_to", "provider_thread_id", "idempotency_key"]);
                if let Some(r) = p.get("references").and_then(references_text) {
                    job.insert("references".into(), json!(r));
                }
                p.id("account_id")?;
                p.opt_id("lead_id")?;
                p.opt_id("campaign_id")?;
                send_job(job, who)
            }
            Op::EmailReply => {
                let msg = p.obj("message")?.ok_or_else(|| bad("`message` is missing. Use `${{ trigger.message }}`."))?;
                let m = P { m: &msg };
                let mid = m.opt_str("message_id_header");
                let mut job = Map::new();
                job.insert("account_id".into(), json!(m.id("account_id")?));
                job.insert("to_email".into(), json!(m.str("from_email")?));
                if let Some(id) = &mid {
                    job.insert("in_reply_to".into(), json!(id));
                }
                // the thread so far, then the message we answer
                let mut refs: Vec<String> = msg.get("references_ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
                if let Some(id) = &mid {
                    if !refs.contains(id) {
                        refs.push(id.clone());
                    }
                }
                if !refs.is_empty() {
                    job.insert("references".into(), json!(refs.join(" ")));
                }
                for k in ["provider_thread_id", "lead_id", "campaign_id"] {
                    if let Some(v) = msg.get(k).filter(|v| !v.is_null()) {
                        job.insert(k.into(), v.clone());
                    }
                }
                job.insert("priority".into(), json!(100));
                job.insert("subject".into(), json!(p.str("subject")?));
                job.extend(p.pick(&["body_text", "body_html"]));
                send_job(job, who)
            }
            Op::ReplyList => {
                let mut req = CallRequest::new("GET", "/v1/replies/messages");
                for k in ["label", "account_id", "lead_id", "campaign_id", "since", "handled"] {
                    if let Some(t) = p.get(k).and_then(query_text).filter(|t| !t.is_empty()) {
                        req = req.query(k, t);
                    }
                }
                plan(req, Shape::Paged { key: "messages", max: p.max_results(100)? })
            }
            Op::ReplySetLabel => {
                let label = p.str("label")?;
                plan(body(CallRequest::new("PATCH", format!("/v1/replies/messages/{}", p.id("message_id")?)), json!({ "label": label }).as_object().cloned().unwrap_or_default()), Shape::Body)
            }
            Op::SuppressionAdd => {
                let value = p.str("value")?.to_lowercase();
                let kind = if value.contains('@') { "email" } else { "domain" };
                let mut b = json!({ "value": value, "kind": kind }).as_object().cloned().unwrap_or_default();
                b.extend(p.pick(&["reason"]));
                plan(body(CallRequest::new("POST", "/v1/sending/suppressions"), b), Shape::Body)
            }
            Op::EnrollmentStop => plan(CallRequest::new("POST", format!("/v1/followups/enrollments/{}/stop", p.id("enrollment_id")?)).body(json!({})), Shape::Body),
            Op::CampaignTest => {
                let b = json!({ "lead_id": p.id("lead_id")?, "step": p.int("step")?.unwrap_or(1) });
                plan(CallRequest::new("POST", format!("/v1/campaigns/{}/emails/test", p.id("campaign_id")?)).body(b), Shape::Body)
            }
            Op::VerifyReoon => {
                let ids = p.get("lead_ids").filter(|v| v.is_array()).ok_or_else(|| bad("`lead_ids` must be a list of ids"))?;
                plan(CallRequest::new("POST", "/v1/verify/reoon/runs").body(json!({ "lead_ids": ids })), Shape::Body)
            }
            Op::Api => self.plan_api(p, who),
        }
    }

    fn plan_api(&self, p: &P, who: &PlanCtx) -> Result<Plan, NodeError> {
        let op_name = p.str("operation")?;
        let ops = who.ops.ok_or_else(|| NodeError::new("no_environment", "The list of API operations is not ready."))?;
        let op = match ops.find(&op_name) {
            Some(o) => o,
            None => {
                let names: Vec<&str> = ops.all().map(|o| o.id.as_str()).collect();
                let near = closest(&op_name, names, 3);
                let hint = if near.is_empty() { String::new() } else { format!(" Did you mean {}?", near.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")) };
                return Err(NodeError::new("unknown_operation", format!("There is no API operation `{op_name}`.{hint} Use `METHOD /path`, for example `GET /v1/leads/{{id}}`.")));
            }
        };
        if let Some(why) = deny_reason(&op.method, &op.path, who.allow_lead_erase) {
            return Err(NodeError { code: "endpoint_not_allowed".into(), message: format!("{} {}: {why}", op.method, op.path), status: Some(403) });
        }
        let path_values = p.obj("path")?.unwrap_or_default();
        let path = fill_path(&op.path, &path_values).map_err(bad)?;
        let mut req = CallRequest::new(&op.method, path);
        if let Some(q) = p.obj("query")? {
            for (k, v) in q {
                if let Some(t) = query_text(&v) {
                    req = req.query(&k, t);
                }
            }
        }
        if let Some(b) = p.get("body") {
            if op.method == "GET" {
                return Err(bad("A GET operation takes no `body`."));
            }
            req = req.body(b.clone());
        } else if op.has_body && op.method != "GET" {
            // routes with a body reject an empty request; send an empty object
            req = req.body(json!({}));
        }
        let split = p.opt_str("split");
        Ok(Plan { req, shape: Shape::Split(split) })
    }
}

/// What the plan needs from the run.
struct PlanCtx<'a> {
    run_id: &'a str,
    node_id: &'a str,
    index: usize,
    ops: Option<&'a crate::automations::internal_call::OperationIndex>,
    allow_lead_erase: bool,
}

/// One job for `POST /v1/sending/jobs`. A default idempotency key makes a repeat of the same item harmless.
fn send_job(mut job: Map<String, Value>, who: &PlanCtx) -> Result<Plan, NodeError> {
    job.entry("idempotency_key").or_insert_with(|| json!(format!("wf:{}:{}:{}", who.run_id, who.node_id, who.index)));
    job.insert("kind".into(), json!("manual"));
    let req = CallRequest::new("POST", "/v1/sending/jobs").body(json!({ "jobs": [job] }));
    Ok(Plan { req, shape: Shape::First { key: "items", pick: &["status", "job", "error"] } })
}

// ---------- running a plan ----------

fn as_object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        Value::Null => Map::new(),
        other => json!({ "value": other }).as_object().cloned().unwrap_or_default(),
    }
}

/// The node error for a refused call: `aikido_<code>` with the status and the message of the route.
fn call_error(r: &CallResponse) -> NodeError {
    let code = r.body.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
    let message = r.body.pointer("/error/message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("The route answered with status {}.", r.status));
    let code = if code.is_empty() { format!("aikido_http_{}", r.status) } else { format!("aikido_{code}") };
    NodeError { code, message, status: Some(r.status) }
}

async fn call(env: &NodeEnv, req: CallRequest) -> Result<CallResponse, NodeError> {
    env.count_call()?;
    let r = env.internal.call(&env.who, req).await?;
    if !r.ok() {
        return Err(call_error(&r));
    }
    Ok(r)
}

async fn execute(env: &NodeEnv, plan: Plan) -> Result<Vec<Map<String, Value>>, NodeError> {
    match plan.shape {
        Shape::Body => Ok(vec![as_object(call(env, plan.req).await?.body)]),
        Shape::First { key, pick } => {
            let r = call(env, plan.req).await?;
            let first = r.body.get(key).and_then(Value::as_array).and_then(|a| a.first()).cloned().ok_or_else(|| NodeError::new("aikido_bad_answer", format!("The answer has no `{key}` entry.")))?;
            let m = as_object(first);
            Ok(vec![pick.iter().map(|k| ((*k).to_string(), m.get(*k).cloned().unwrap_or(Value::Null))).collect()])
        }
        Shape::Paged { key, max } => {
            let mut rows: Vec<Map<String, Value>> = vec![];
            let mut cursor: Option<String> = None;
            loop {
                let want = (max - rows.len()).min(PAGE_SIZE);
                let mut req = plan.req.clone().query("limit", want);
                if let Some(c) = &cursor {
                    req = req.query("cursor", c);
                }
                let r = call(env, req).await?;
                let list = r.body.get(key).and_then(Value::as_array).ok_or_else(|| NodeError::new("aikido_bad_answer", format!("The answer has no `{key}` list.")))?;
                rows.extend(list.iter().take(max - rows.len()).cloned().map(as_object));
                cursor = r.body.get("next_cursor").and_then(query_text).filter(|c| !c.is_empty());
                if rows.len() >= max || cursor.is_none() || list.is_empty() {
                    break;
                }
            }
            Ok(rows)
        }
        Shape::Split(path) => {
            let r = call(env, plan.req).await?;
            match path {
                None => Ok(vec![as_object(r.body)]),
                Some(path) => {
                    let list = r.body.as_object().and_then(|o| get_path(o, &path)).and_then(Value::as_array).ok_or_else(|| NodeError::new("split_not_a_list", format!("`{path}` is not a list in the answer.")))?;
                    Ok(list.iter().cloned().map(as_object).collect())
                }
            }
        }
    }
}

impl Aikido {
    /// The params of one item, evaluated, as a plan.
    fn plan_for(&self, ctx: &NodeCtx<'_>, item: &Item, idx: usize) -> Result<Plan, NodeError> {
        let params = match ctx.value(item, idx, &Value::Object(ctx.params.clone()))? {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        // a dry run may have no server services: the plan still shows the call
        let pc = PlanCtx {
            run_id: &ctx.eval.facts().run_id,
            node_id: ctx.node_id,
            index: idx,
            ops: ctx.env.map(|e| e.ops.as_ref()),
            allow_lead_erase: ctx.env.is_some_and(|e| e.settings.allow_lead_erase),
        };
        self.plan(&P { m: &params }, &pc)
    }
}

#[async_trait]
impl NodeType for Aikido {
    fn spec(&self) -> &NodeSpec {
        &self.spec
    }

    /// `aikido.api` changes data unless the operation is a GET. An operation that is not a plain text, or that
    /// the API does not know, counts as a change (the safe answer).
    fn side_effect(&self, params: &Map<String, Value>) -> bool {
        if self.op != Op::Api {
            return self.spec.side_effect;
        }
        let Some(name) = params.get("operation").and_then(Value::as_str).filter(|s| !s.contains("${")) else { return true };
        match crate::automations::internal_call::KNOWN_OPERATIONS.get().and_then(|ops| ops.find(name)) {
            Some(op) => op.method != "GET",
            None => true,
        }
    }

    fn dry(&self, ctx: &NodeCtx<'_>, item: &Item, index: usize) -> Option<Result<Value, NodeError>> {
        Some(self.plan_for(ctx, item, index).map(|p| p.req.would_call()))
    }

    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        let env = ctx.env()?;
        let item = input.items.first().ok_or_else(|| bad("no input item"))?;
        let plan = self.plan_for(ctx, item, input.first_index)?;
        let rows = execute(env, plan).await?;
        Ok(NodeOutput::main(rows.into_iter().map(|m| item.child(m)).collect()))
    }
}
