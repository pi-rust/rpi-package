//! Immutable snapshot, native install/create, then durable test identity.
use super::super::{
    AuthoringTest, LocalError, LocalResourceOperation, LocalResourceRequest, LocalWorkspace,
    MainPackage, NativeKind, Path, PathBuf, ResourceKind, ResourceScope, ResourceSelection,
    ResourceTools, Value, WorkspaceError, conflict, decode, digest, immutable, invalid, json,
    nonce, read_sources, resource_error, save_store, service, skill_name, sources_digest,
    write_sources,
};
use super::Transaction;
struct PreparedTest {
    draft_id: String,
    id: String,
    revision: u64,
    digest: String,
    snapshot: PathBuf,
    journal: PathBuf,
    kind: NativeKind,
    name: String,
}
impl Transaction<'_> {
    pub(super) async fn test(
        &mut self,
        id: String,
        revision: u64,
        expected: String,
    ) -> Result<(), WorkspaceError> {
        let prepared = self.prepare_test(id, revision, expected)?;
        // Native management/create own lifecycle; never recursively retain it.
        drop(self.fence.take());
        let selection = self.install_test(&prepared).await?;
        let (session_id, readback) = self
            .create_test_session(selection, &prepared.journal)
            .await?;
        let draft = self
            .state
            .drafts
            .iter_mut()
            .find(|d| d.id == prepared.draft_id)
            .ok_or_else(|| invalid("unknown draft"))?;
        draft.tests.push(AuthoringTest {
            id: prepared.id,
            revision: prepared.revision,
            digest: prepared.digest,
            snapshot: prepared.snapshot.to_string_lossy().into_owned(),
            session_id,
            readback,
            evidence: None,
        });
        // Metadata-only publication requires serial authoring, not a new source
        // fence which a racing prompt could refuse after native creation.
        save_store(&self.root.join("store.json"), &self.state)?;
        immutable(
            &prepared.journal.join("completed.json"),
            b"{\"version\":1,\"recorded\":true}",
        )?;
        Ok(())
    }
    fn prepare_test(
        &self,
        id: String,
        revision: u64,
        expected: String,
    ) -> Result<PreparedTest, WorkspaceError> {
        let draft = self
            .state
            .drafts
            .iter()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid("unknown draft"))?;
        LocalWorkspace::check_revision(draft, revision, &expected)?;
        if !draft
            .validation
            .as_ref()
            .is_some_and(|v| v.valid && v.revision == revision && v.digest == expected)
        {
            return Err(conflict("validate the current saved bytes before testing"));
        }
        if draft.tests.len() >= 32 {
            return Err(invalid("test record capacity exhausted"));
        }
        let test_id = format!("test-{}", nonce());
        let snapshot = self
            .owner
            .inner
            .data
            .join("artifacts")
            .join(self.project)
            .join(&id)
            .join(&test_id);
        write_sources(&snapshot, &draft.sources)?;
        if sources_digest(&read_sources(&snapshot)?)? != expected {
            return Err(conflict("snapshot bytes differ"));
        }
        // Native Skill name == installed directory. Never transform SOURCE to
        // fit a random ID; incompatible existing installations refuse normally.
        let name = if draft.kind == NativeKind::Skill {
            skill_name(&draft.sources)?
        } else {
            format!("work-{}", &digest(test_id.as_bytes())[..24])
        };
        let journal = self.root.join("test-journal").join(&test_id);
        immutable(&journal.join("intent.json"), &serde_json::to_vec(&json!({"version":1,"testId":test_id,"draftId":id,"revision":revision,"digest":expected,"snapshot":snapshot})).map_err(LocalError::from)?)?;
        Ok(PreparedTest {
            draft_id: id,
            id: test_id,
            revision,
            digest: expected,
            snapshot,
            journal,
            kind: draft.kind,
            name,
        })
    }
    async fn install_test(&self, test: &PreparedTest) -> Result<ResourceSelection, WorkspaceError> {
        let result = self
            .service
            .manage_resources(LocalResourceRequest {
                project_id: self.project.into(),
                scope: ResourceScope::Project,
                operation: LocalResourceOperation::Install {
                    kind: match test.kind {
                        NativeKind::Package => ResourceKind::Package,
                        NativeKind::Skill => ResourceKind::Skill,
                        NativeKind::Extension => ResourceKind::Extension,
                    },
                    name: test.name.clone(),
                    source: test.snapshot.clone(),
                    version: Some(test.digest.clone()),
                },
            })
            .await
            .map_err(resource_error)?;
        immutable(
            &test.journal.join("install-receipt.json"),
            &serde_json::to_vec(&result).map_err(LocalError::from)?,
        )?;
        if !result.success {
            return Err(conflict(&format!(
                "snapshot install refused; explicitly release idle hosts then retry: {}",
                result.observation
            )));
        }
        if test.kind == NativeKind::Package {
            Ok(ResourceSelection {
                version: 1,
                main_package: Some(MainPackage {
                    name: test.name.clone(),
                    scope: Some(ResourceScope::Project),
                    instructions: vec!["prompts/system.md".into()],
                }),
                skills: vec![],
                extensions: vec![],
                instructions: vec![],
                tools: ResourceTools::Default {},
            })
        } else {
            let mut selection = self.owner.shipped_selection(&self.service, false).await?;
            let path = result.observation["data"]["resource"]["selectable"]["skills"][0]
                .as_str()
                .ok_or_else(|| invalid("native Skill receipt missing entry"))?;
            selection.skills.push(path.into());
            Ok(selection)
        }
    }
    async fn create_test_session(
        &self,
        selection: ResourceSelection,
        journal: &Path,
    ) -> Result<(String, Value), WorkspaceError> {
        let intended_selection = selection.clone();
        let result: Value = decode(
            service::create_session(
                &self.service,
                service::CreateSessionRequest {
                    project_id: Some(self.project.into()),
                    resources: Some(selection),
                },
            )
            .await,
        )?;
        // Publish recoverable identity BEFORE readback/await can be refused.
        // Caller cancellation cannot abandon this retained worker's publication.
        immutable(
            &journal.join("session.json"),
            &serde_json::to_vec(&result).map_err(LocalError::from)?,
        )?;
        let session_id = result["sessionId"]
            .as_str()
            .ok_or_else(|| invalid("missing test session"))?
            .to_owned();
        let host = self
            .service
            .live_session(&session_id)
            .await
            .ok_or_else(|| conflict("test host unavailable"))?;
        let readback = host.resource_state().await.map_err(resource_error)?.body;
        if self
            .service
            .resource_configuration(&session_id)
            .await
            .map_err(resource_error)?
            .is_none_or(|c| c.selection != intended_selection)
        {
            return Err(conflict(
                "test configuration changed before publication; recover identity from test-journal/session.json",
            ));
        }
        Ok((session_id, readback))
    }
}
