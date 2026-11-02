use super::*;
#[cfg(test)]
mod tests;
pub(super) fn planned_record(
    prior: Option<&ResourceConfiguration>,
    request: &ApplyResourcesRequest,
    expected: ExpectedBinding,
    live_settings: Option<ResourceSessionSettings>,
    superseding: Option<usize>,
) -> ResourceResult<ResourceConfiguration> {
    let restore_settings = live_settings
        .clone()
        .or_else(|| {
            superseding
                .and_then(|index| prior.and_then(|r| r.operations[index].restore_settings.clone()))
        })
        .or_else(|| {
            prior
                .and_then(|r| r.current_settings.as_ref())
                .map(|c| c.settings.clone())
        });
    let settings =
        live_settings.unwrap_or_else(|| prior.map(|r| r.settings.clone()).unwrap_or_default());
    // An unbound legacy record is ONLY an intent journal, never an active
    // configuration. The public configuration API returns None until Applied.
    let mut next = prior.cloned().unwrap_or(ResourceConfiguration {
        selection: request.target.clone(),
        revision: 0,
        settings: settings.clone(),
        current_settings: None,
        observation: None,
        operations: vec![],
    });
    next.settings = settings;
    if let Some(index) = superseding {
        next.operations[index].status = ResourceOperationStatus::Superseded;
        next.operations[index].error = Some(format!(
            "explicitly superseded by {} after canonical noncommit proof",
            request.operation_id
        ));
    }
    let transition = ResourceTransition {
        version: 1,
        operation_id: request.operation_id.clone(),
        expected,
        target: request.target.clone(),
    };
    if serde_json::to_vec(&transition)
        .map_err(ResourceError::unavailable)?
        .len()
        > MAX_CONFIG_BYTES
    {
        return Err(ResourceError::new(
            ResourceErrorKind::LimitExceeded,
            "native transition request exceeds 64 KiB",
        ));
    }
    next.operations.push(ResourceOperation {
        request: request.clone(),
        transition,
        restore_settings,
        status: ResourceOperationStatus::Pending,
        result_revision: None,
        observation: None,
        error: None,
    });
    next.validate()?;
    require_result_headroom(&next)?;
    Ok(next)
}
pub(super) fn require_result_headroom(record: &ResourceConfiguration) -> ResourceResult<()> {
    if serde_json::to_vec(record)
        .map_err(ResourceError::unavailable)?
        .len()
        > RECORD_CAP - RESULT_HEADROOM
    {
        return Err(ResourceError::new(
            ResourceErrorKind::LimitExceeded,
            "resource ledger lacks worst-case result headroom",
        ));
    }
    Ok(())
}
