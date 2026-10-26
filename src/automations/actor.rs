//! Who made a change: a lead timeline event written while an automation node calls a route carries
//! `detail.actor = "automation:<workflow id>"` and `detail.automation_run_id` (build map 8.8 and 9.3).
//! The event triggers use it to skip a workflow's own changes.
//!
//! The engine calls the routes in-process, in the same task, so a task-local value reaches the event writers
//! without a change to any route.
use serde_json::{Value, json};
use std::future::Future;

#[derive(Clone, Debug)]
pub struct Actor {
    pub workflow_id: String,
    pub run_id: String,
}

tokio::task_local! {
    static ACTOR: Actor;
}

/// Run `f` as this actor.
pub async fn scope<F: Future>(actor: Actor, f: F) -> F::Output {
    ACTOR.scope(actor, f).await
}

/// The actor of the current call, if an automation node makes it.
pub fn current() -> Option<Actor> {
    ACTOR.try_with(Clone::clone).ok()
}

/// Add the actor to the `detail` of a lead event. Does nothing outside an automation call.
pub fn tag(mut detail: Value) -> Value {
    if let (Some(a), Some(o)) = (current(), detail.as_object_mut()) {
        o.insert("actor".into(), json!(format!("automation:{}", a.workflow_id)));
        o.insert("automation_run_id".into(), json!(a.run_id));
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_actor_is_set_only_inside_the_scope() {
        assert_eq!(tag(json!({ "a": 1 })), json!({ "a": 1 }));
        let out = scope(Actor { workflow_id: "w1".into(), run_id: "r1".into() }, async {
            tokio::task::yield_now().await;
            tag(json!({ "a": 1 }))
        })
        .await;
        assert_eq!(out, json!({ "a": 1, "actor": "automation:w1", "automation_run_id": "r1" }));
        assert!(current().is_none());
        // a value that is not an object is left alone
        let out = scope(Actor { workflow_id: "w1".into(), run_id: "r1".into() }, async { tag(json!("text")) }).await;
        assert_eq!(out, json!("text"));
    }
}
