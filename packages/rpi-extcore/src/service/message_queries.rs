//! Capability-gated bounded native reads; never emulate them with a transcript scan.
use super::Service;
use crate::reply::{failure, IntoReply, ReplyKind, ServiceReply, ServiceResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
fn summary_limit() -> usize {
    10
}
fn window_limit() -> usize {
    48
}
fn user_role() -> String {
    "user".into()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageSummaryQuery {
    #[serde(default = "summary_limit")]
    pub limit: usize,
    #[serde(default = "user_role")]
    pub role: String,
    pub before_id: Option<String>,
    pub leaf_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageWindowQuery {
    #[serde(default = "window_limit")]
    pub limit: usize,
    pub anchor_id: Option<String>,
    pub before_id: Option<String>,
    pub after_id: Option<String>,
    pub leaf_id: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub entry_id: String,
    pub role: String,
    pub preview: String,
    pub timestamp: i64,
    pub has_images: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummaryPage {
    pub filtered: bool,
    pub role: String,
    pub messages: Vec<MessageSummary>,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub next_before_id: Option<String>,
    pub has_more: bool,
    pub leaf_id: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageWindowPage {
    pub messages: Vec<Value>,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub next_before_id: Option<String>,
    pub next_after_id: Option<String>,
    pub has_more_before: bool,
    pub has_more_after: bool,
    pub leaf_id: Option<String>,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
}
fn validate(limit: usize, ids: &[Option<&str>]) -> ServiceResult<()> {
    if !(1..=100).contains(&limit) || ids.iter().flatten().any(|id| !valid_id(id)) {
        return Err(failure(
            ReplyKind::InvalidInput,
            "limit must be 1..100 and cursors must be nonempty entry IDs",
        ));
    }
    Ok(())
}
async fn read<Q: Serialize>(state: &Service, id: &str, command: &str, query: &Q) -> ServiceResult {
    let session = state
        .sessions
        .read()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| failure(ReplyKind::NotFound, "session not found"))?;
    if !session.supports_command(command) {
        return Err((
            ReplyKind::Unsupported,
            json!({"code":"message_queries_unsupported", "error":"native host does not advertise this bounded message query"}),
        ));
    }
    let mut request = serde_json::to_value(query)
        .map_err(|_| failure(ReplyKind::Internal, "cannot encode message query"))?;
    // Optional absent cursors must be omitted, not emitted as null URL values.
    request
        .as_object_mut()
        .unwrap()
        .retain(|_, value| !value.is_null());
    request["type"] = json!(command);
    request["id"] = json!(uuid::Uuid::new_v4().to_string());
    let response = session
        .send(request)
        .await
        .map_err(|error| failure(ReplyKind::HostUnavailable, &error))?;
    if response["success"] != true {
        let error = response["error"]
            .as_str()
            .unwrap_or("native message query failed");
        return Err((
            if error.contains("branch_changed:") {
                ReplyKind::Conflict
            } else {
                ReplyKind::InvalidInput
            },
            json!({"error":error, "code":if error.contains("branch_changed:") {"branch_changed"} else {"message_query_failed"}}),
        ));
    }
    Ok(response["data"].clone())
}
fn malformed() -> crate::reply::ServiceError {
    failure(
        ReplyKind::HostUnavailable,
        "native host returned an invalid bounded message page",
    )
}
fn validate_edges(
    ids: &[&str],
    first: Option<&str>,
    last: Option<&str>,
    leaf: Option<&str>,
    pinned: Option<&str>,
) -> ServiceResult<()> {
    let unique: HashSet<_> = ids.iter().copied().collect();
    if unique.len() != ids.len()
        || ids.iter().any(|id| !valid_id(id))
        || first != ids.first().copied()
        || last != ids.last().copied()
        || (!ids.is_empty() && !leaf.is_some_and(valid_id))
        || pinned.is_some_and(|pinned| Some(pinned) != leaf)
    {
        return Err(malformed());
    }
    Ok(())
}
pub async fn message_summaries(
    state: &Service,
    id: String,
    query: MessageSummaryQuery,
) -> ServiceReply {
    async {
        validate(
            query.limit,
            &[query.before_id.as_deref(), query.leaf_id.as_deref()],
        )?;
        if query.role != "user" {
            return Err(failure(
                ReplyKind::InvalidInput,
                "only role=user is supported",
            ));
        }
        let data = read(state, &id, "get_message_summaries", &query).await?;
        let page: MessageSummaryPage = serde_json::from_value(data).map_err(|_| malformed())?;
        if !page.filtered
            || page.role != "user"
            || page.messages.len() > query.limit
            || page
                .messages
                .iter()
                .any(|message| message.role != "user" || message.preview.chars().count() > 140)
            || page.next_before_id.as_ref()
                != if page.has_more {
                    page.first_id.as_ref()
                } else {
                    None
                }
            || (page.has_more && page.messages.is_empty())
            || page.first_id == query.before_id && query.before_id.is_some()
        {
            return Err(malformed());
        }
        validate_edges(
            &page
                .messages
                .iter()
                .map(|message| message.entry_id.as_str())
                .collect::<Vec<_>>(),
            page.first_id.as_deref(),
            page.last_id.as_deref(),
            page.leaf_id.as_deref(),
            query.leaf_id.as_deref(),
        )?;
        serde_json::to_value(page).map_err(|_| malformed())
    }
    .await
    .into_reply()
}
pub async fn message_window(
    state: &Service,
    id: String,
    query: MessageWindowQuery,
) -> ServiceReply {
    async {
        validate(
            query.limit,
            &[
                query.anchor_id.as_deref(),
                query.before_id.as_deref(),
                query.after_id.as_deref(),
                query.leaf_id.as_deref(),
            ],
        )?;
        if [&query.anchor_id, &query.before_id, &query.after_id]
            .iter()
            .filter(|id| id.is_some())
            .count()
            != 1
        {
            return Err(failure(
                ReplyKind::InvalidInput,
                "exactly one of anchorId, beforeId, afterId is required",
            ));
        }
        let page: MessageWindowPage =
            serde_json::from_value(read(state, &id, "get_message_window", &query).await?)
                .map_err(|_| malformed())?;
        let ids = page
            .messages
            .iter()
            .map(|message| message["entryId"].as_str().unwrap_or(""))
            .collect::<Vec<_>>();
        if ids.len() > query.limit
            || query
                .anchor_id
                .as_ref()
                .is_some_and(|id| !ids.contains(&id.as_str()))
            || query
                .before_id
                .as_ref()
                .is_some_and(|id| ids.contains(&id.as_str()))
            || query
                .after_id
                .as_ref()
                .is_some_and(|id| ids.contains(&id.as_str()))
            || page.next_before_id.as_ref()
                != if page.has_more_before {
                    page.first_id.as_ref()
                } else {
                    None
                }
            || page.next_after_id.as_ref()
                != if page.has_more_after {
                    page.last_id.as_ref()
                } else {
                    None
                }
            || (ids.is_empty() && (page.has_more_before || page.has_more_after))
        {
            return Err(malformed());
        }
        validate_edges(
            &ids,
            page.first_id.as_deref(),
            page.last_id.as_deref(),
            page.leaf_id.as_deref(),
            query.leaf_id.as_deref(),
        )?;
        serde_json::to_value(page).map_err(|_| malformed())
    }
    .await
    .into_reply()
}

#[cfg(test)]
mod tests;
