//! Additive native identity for live proofs; frozen v1 query pages stay unchanged.
use super::{tool_program_access::ChildAccess, tool_program_queries as query};
use crate::{
    host::HostSession,
    reply::{failure, ReplyKind, ServiceResult},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

const COMMAND: &str = "get_tool_program_branch";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BranchIdentity {
    session_id: String,
    lane: String,
    binding_id: String,
    generation: u64,
    pub(super) leaf_id: Option<String>,
}
impl BranchIdentity {
    pub(super) fn same_branch(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.lane == other.lane
            && self.binding_id == other.binding_id
            && self.generation == other.generation
    }
}

pub(super) fn supported(host: &HostSession) -> bool {
    host.ready()["toolProgramBranch"]
        == json!({"version":1,"command":COMMAND,
        "identity":"bindingAndLaneGeneration"})
        && host.supports_command(COMMAND)
}

pub(super) async fn read(access: &ChildAccess) -> ServiceResult<BranchIdentity> {
    if !supported(&access.host) {
        return Err(failure(
            ReplyKind::Unsupported,
            "native host does not advertise live child branch identity",
        ));
    }
    let identity: BranchIdentity = query::read(
        query::QueryOwner::Bound(access),
        &access.id,
        COMMAND,
        &json!({}),
        2048,
    )
    .await?;
    let valid_binding = uuid::Uuid::parse_str(&identity.binding_id)
        .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == identity.binding_id);
    if identity.session_id != access.id
        || identity.lane != "main"
        || !valid_binding
        || identity
            .leaf_id
            .as_deref()
            .is_some_and(|id| !query::valid_id(id))
    {
        return Err(failure(
            ReplyKind::HostUnavailable,
            "invalid native child branch identity",
        ));
    }
    Ok(identity)
}
