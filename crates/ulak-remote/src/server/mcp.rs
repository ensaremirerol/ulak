//! MCP streamable HTTP, POST only: a hand-written JSON-RPC subset answered as `application/json`
//! (no SSE, no batches, no sessions; GET and DELETE on `/mcp` are 405).
use super::hub::{now_ms, Hub};
use crate::*;
use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use ulak_proto::*;

pub(crate) async fn handle(State(h): State<Arc<Hub>>, headers: HeaderMap, body: Bytes) -> Response {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|t| AgentToken::new(t.to_owned()).ok());
    // Sender identity comes only from here.
    let Some(from) = bearer.and_then(|t| AgentTokens::verify(&*h.store, &t, now_ms()).ok()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        let mut r = rpc_err(Value::Null, -32700, "parse error");
        *r.status_mut() = StatusCode::BAD_REQUEST;
        return r;
    };
    // Notifications and client responses get 202 without a body.
    let (Some(id), true) = (req.get("id").cloned(), req.get("method").is_some()) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    match req.get("method").and_then(Value::as_str) {
        Some("initialize") => rpc_ok(
            id,
            json!({"protocolVersion": negotiate(&params), "capabilities": {"tools": {}},
                   "serverInfo": {"name": "ulak-hub", "version": env!("CARGO_PKG_VERSION")}}),
        ),
        Some("ping") => rpc_ok(id, json!({})),
        Some("tools/list") => rpc_ok(id, json!({"tools": tools(h.can_forward(&from))})),
        Some("tools/call") => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            // Operator trace: who called which tool (never arguments or results).
            eprintln!("mcp: {} tools/call {name}", from.as_str());
            let result = match call(&h, &from, name, args).await {
                Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
                Err(e) => {
                    json!({"content": [{"type": "text", "text": e.to_string()}], "isError": true})
                }
            };
            rpc_ok(id, result)
        }
        _ => rpc_err(id, -32601, "method not found"),
    }
}

/// Versions we speak; the first is the latest.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// The client's requested version if supported, else our latest.
fn negotiate(params: &Value) -> &'static str {
    let ask = params.get("protocolVersion").and_then(Value::as_str);
    VERSIONS
        .into_iter()
        .find(|v| Some(*v) == ask)
        .unwrap_or(VERSIONS[0])
}

fn rpc_ok(id: Value, result: Value) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}
fn rpc_err(id: Value, code: i32, message: &str) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
        .into_response()
}

/// `route`: the agent has the `ulak-route` skill and also gets `ulak_forward_issue`.
fn tools(route: bool) -> Value {
    let obj =
        |props: Value, req: Value| json!({"type": "object", "properties": props, "required": req});
    let mut t = json!([
        {"name": "send_message", "description": "Send a message to another agent.",
         "inputSchema": obj(json!({"to": {"type": "string"}, "body": {"type": "string"}, "thread": {"type": "string"}}), json!(["to", "body"]))},
        {"name": "read_inbox", "description": "Read your pending messages, oldest first.",
         "inputSchema": obj(json!({"limit": {"type": "integer"}}), json!([]))},
        {"name": "ask_human", "description": "Ask the human operator a question. Blocks up to 10 minutes; if unanswered you get a question_id and will be nudged when the answer arrives.",
         "inputSchema": obj(json!({"question": {"type": "string"}}), json!(["question"]))},
        {"name": "check_answer", "description": "Fetch the answer to an earlier ask_human question.",
         "inputSchema": obj(json!({"question_id": {"type": "string"}}), json!(["question_id"]))},
    ]);
    if route {
        if let Some(a) = t.as_array_mut() {
            a.push(json!({"name": "ulak_forward_issue",
                "description": "Hand a GitHub issue, or part of your task, to the agent that owns that area. to: \"auto\" (routing rules decide), an agent name, or \"new\" (start a new agent). Returns {forwarded_to} or {waiting_for: \"you\"}.",
                "inputSchema": obj(json!({"issue": {"type": "string", "description": "owner/name#number"}, "to": {"type": "string"}, "reason": {"type": "string"}}), json!(["issue", "to", "reason"]))}));
        }
    }
    t
}

#[derive(Deserialize)]
struct SendArgs {
    to: AgentId,
    body: String,
    thread: Option<ThreadId>,
}
#[derive(Deserialize)]
struct InboxArgs {
    limit: Option<u32>,
}
#[derive(Deserialize)]
struct AskArgs {
    question: String,
}
#[derive(Deserialize)]
struct ForwardArgs {
    issue: String,
    to: String,
    reason: String,
}
#[derive(Deserialize)]
struct CheckArgs {
    question_id: QuestionId,
}

fn args<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ServerError> {
    serde_json::from_value(v).map_err(|e| ServerError::BadArgs(e.to_string()))
}

async fn call(h: &Arc<Hub>, from: &AgentId, name: &str, a: Value) -> Result<String, ServerError> {
    let now = now_ms();
    match name {
        "send_message" => {
            let a: SendArgs = args(a)?;
            let m = Mailbox::send(&*h.store, from, &a.to, a.thread, a.body, now)?;
            let out = json!({"message_id": m.id(), "thread": m.thread()}).to_string();
            h.ui_send(HubToUi::MessageLogged(m));
            Ok(out)
        }
        "read_inbox" => {
            let a: InboxArgs = args(a)?;
            let msgs = Mailbox::inbox(&*h.store, from, a.limit.unwrap_or(20).min(100))?;
            Ok(json!(msgs
                .iter()
                .map(|m| json!({"id": m.id(), "from": m.from(), "thread": m.thread(), "body": m.payload()}))
                .collect::<Vec<_>>())
            .to_string())
        }
        "ask_human" => {
            let a: AskArgs = args(a)?;
            // Subscribe before opening so an instant answer is not missed.
            let mut rx = h.answered.subscribe();
            let q = Questions::open(&*h.store, from, a.question, now)?;
            let id = q.id().clone();
            h.ui_send(HubToUi::QuestionOpened(q));
            let _ = tokio::time::timeout(h.cfg.ask_timeout, async {
                loop {
                    match rx.recv().await {
                        Ok(i) if i == id => break,
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => break,
                    }
                }
            })
            .await;
            answer_or_pending(h, &id)
        }
        "check_answer" => {
            let a: CheckArgs = args(a)?;
            // Another agent's question looks like a missing one.
            if &Questions::asker(&*h.store, &a.question_id)? != from {
                return Err(QuestionError::NotFound(a.question_id).into());
            }
            answer_or_pending(h, &a.question_id)
        }
        "ulak_forward_issue" => {
            let a: ForwardArgs = args(a)?;
            let (h, from) = (h.clone(), from.clone());
            tokio::task::spawn_blocking(move || h.forward_issue(&from, &a.issue, &a.to, &a.reason))
                .await
                .map_err(|_| ServerError::BadArgs("internal error".into()))?
                .map(|v| v.to_string())
        }
        other => Err(ServerError::BadArgs(format!("unknown tool {other:?}"))),
    }
}

fn answer_or_pending(h: &Hub, id: &QuestionId) -> Result<String, ServerError> {
    Ok(match Questions::collect(&*h.store, id, now_ms())? {
        Some(a) => json!({"status": "answered", "question_id": id, "answer": a.text()}),
        None => json!({"status": "pending", "question_id": id,
            "message": "No answer yet. You will be nudged in your terminal when it arrives; then call check_answer with this question_id."}),
    }
    .to_string())
}
