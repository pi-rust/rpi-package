//! Short-lived authoring transaction inside the retained blocking worker.
//! Core owns admission; this value keeps its guard alive across source IO.
use super::{
    AuthoringDraft, AuthoringRelease, AuthoringRequest, AuthoringState, Duration, LocalError,
    LocalWorkspace, MAX_SOURCE, NativeKind, Path, PathBuf, Service, SessionSummary, WorkspaceError,
    conflict, decode, digest, identifier, immutable, invalid, json, nonce, read_sources,
    resource_error, save_store, service, sources_digest, templates, validation, write_sources,
};
mod testing;
use rpi_extcore::service::ResourceProjectFence;
use rpi_node_api::api::NativeCatalog;
use std::{collections::BTreeMap, sync::Arc};

pub(super) async fn run(
    owner: &LocalWorkspace,
    project: &str,
    request: AuthoringRequest,
) -> Result<AuthoringState, WorkspaceError> {
    let service = owner.project_service(project)?;
    let root = owner.authoring_root(project)?;
    // Catalog takes core lifecycle itself, so observe before source admission.
    let publication_catalog = if matches!(&request, AuthoringRequest::Publish { .. }) {
        Some(owner.catalog_for(&service).await?)
    } else {
        None
    };
    let fence = Some(owner.authoring_admission(&service, project).await?);
    let mut state = LocalWorkspace::load_authoring(&root)?;
    if LocalWorkspace::sync_authoring(&root, &mut state)? {
        save_store(&root.join("store.json"), &state)?;
    }
    let mut transaction = Transaction {
        owner,
        project,
        service,
        root,
        state,
        fence,
        publication_catalog,
    };
    if transaction.dispatch(request).await? {
        save_store(&transaction.root.join("store.json"), &transaction.state)?;
    }
    // The transaction's fence outlives the final metadata write above.
    Ok(transaction.state)
}
struct Transaction<'a> {
    owner: &'a LocalWorkspace,
    project: &'a str,
    service: Arc<Service>,
    root: PathBuf,
    state: AuthoringState,
    fence: Option<ResourceProjectFence>,
    publication_catalog: Option<NativeCatalog>,
}
impl Transaction<'_> {
    async fn dispatch(&mut self, request: AuthoringRequest) -> Result<bool, WorkspaceError> {
        match request {
            AuthoringRequest::Read => return Ok(false),
            request @ (AuthoringRequest::Create { .. } | AuthoringRequest::ImportLegacy { .. }) => {
                self.create(request)?;
            }
            AuthoringRequest::Save {
                id,
                revision,
                digest,
                sources,
            } => {
                self.save(&id, revision, &digest, sources)?;
                return Ok(false);
            }
            AuthoringRequest::Validate {
                id,
                revision,
                digest,
            } => self.validate(&id, revision, &digest)?,
            AuthoringRequest::Test {
                id,
                revision,
                digest,
            } => {
                self.test(id, revision, digest).await?;
                return Ok(false);
            }
            AuthoringRequest::ExecuteTest {
                id,
                test_id,
                prompt,
            } => self.execute(id, test_id, prompt).await?,
            AuthoringRequest::RecordEvidence { id, test_id } => {
                self.record_evidence(&id, &test_id)?;
                return Ok(false);
            }
            AuthoringRequest::Publish {
                id,
                revision,
                digest,
                test_id,
                version,
            } => self.publish(&id, revision, digest, test_id, version)?,
        }
        Ok(true)
    }
    fn create(&mut self, request: AuthoringRequest) -> Result<(), WorkspaceError> {
        let root = &self.root;
        let state = &mut self.state;
        let (AuthoringRequest::Create { id, .. } | AuthoringRequest::ImportLegacy { id, .. }) =
            &request
        else {
            return Err(invalid("invalid create action"));
        };
        identifier(id)?;
        let id = id.clone();
        if state.drafts.iter().any(|d| d.id == id) || state.drafts.len() >= 64 {
            return Err(conflict("draft identity exists or capacity exhausted"));
        }
        let (kind, files) = match request {
            AuthoringRequest::ImportLegacy { original, .. } => {
                if original.len() > MAX_SOURCE / 2 {
                    return Err(invalid("legacy import exceeds budget"));
                }
                // Keep exact original recoverable bytes; never parse design JSON into execution authority.
                immutable(
                    &root
                        .join("legacy-imports")
                        .join(format!("{}.json", digest(original.as_bytes()))),
                    original.as_bytes(),
                )?;
                let mut files = templates(&id, NativeKind::Package);
                files.insert("references/legacy-inert.json".into(), original);
                (NativeKind::Package, files)
            }
            AuthoringRequest::Create { kind, .. } => (kind, templates(&id, kind)),
            _ => unreachable!(),
        };
        let source = root.join("drafts").join(&id).join("source");
        if source.try_exists().map_err(LocalError::from)? {
            return Err(conflict(
                "unrecorded draft source already exists; retain original bytes and recover explicitly or choose a new ID",
            ));
        }
        write_sources(&source, &files)?;
        state.drafts.push(AuthoringDraft {
            id,
            kind,
            revision: 1,
            digest: sources_digest(&files)?,
            path: source.to_string_lossy().into_owned(),
            sources: files,
            validation: None,
            tests: vec![],
            releases: vec![],
        });
        Ok(())
    }
    fn save(
        &mut self,
        id: &str,
        revision: u64,
        expected: &str,
        sources: BTreeMap<String, String>,
    ) -> Result<(), WorkspaceError> {
        let root = &self.root;
        let state = &mut self.state;
        let draft = state
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid("unknown draft"))?;
        LocalWorkspace::check_revision(draft, revision, expected)?;
        let new_digest = sources_digest(&sources)?;
        if new_digest == draft.digest {
            return Ok(());
        }
        let backup = root.join("recovery").join(format!("{}-{}", id, nonce()));
        write_sources(&backup.join("old"), &draft.sources)?;
        write_sources(&backup.join("new"), &sources)?;
        // Journal is durable BEFORE replacing source. Any uncertain failure refuses future writes.
        immutable(&root.join("pending.json"), &serde_json::to_vec(&json!({"version":1,"id":id,"oldDigest":draft.digest,"newDigest":new_digest,"recovery":backup,"metadata":state})).map_err(LocalError::from)?)?;
        let source = root.join("drafts").join(id).join("source");
        // Rename whole source trees while core admission is held. Noncooperative external writers are not fenced.
        if sources_digest(&read_sources(&source)?)? != expected {
            return Err(conflict(
                "source changed during save; recovery bytes preserved",
            ));
        }
        std::fs::rename(&source, backup.join("replaced-source")).map_err(LocalError::from)?;
        std::fs::rename(backup.join("new"), &source).map_err(LocalError::from)?;
        let draft = state
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid("unknown draft"))?;
        draft.sources = sources;
        draft.digest = new_digest;
        draft.revision += 1;
        draft.validation = None;
        save_store(&root.join("store.json"), state)?;
        std::fs::remove_file(root.join("pending.json")).map_err(LocalError::from)?;
        Ok(())
    }
    fn validate(&mut self, id: &str, revision: u64, expected: &str) -> Result<(), WorkspaceError> {
        let state = &mut self.state;
        let draft = state
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid("unknown draft"))?;
        LocalWorkspace::check_revision(draft, revision, expected)?;
        draft.validation = Some(validation(draft));
        Ok(())
    }
    async fn execute(
        &mut self,
        id: String,
        test_id: String,
        prompt: String,
    ) -> Result<(), WorkspaceError> {
        let root = &self.root;
        let state = &mut self.state;
        let service = &self.service;
        let project = self.project;
        let fence = &mut self.fence;
        if prompt.trim().is_empty() || prompt.len() > 32768 {
            return Err(invalid("test prompt must be nonempty and at most 32 KiB"));
        }
        let draft = state
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid("unknown draft"))?;
        let test = draft
            .tests
            .iter_mut()
            .find(|t| t.id == test_id)
            .ok_or_else(|| invalid("unknown test"))?;
        if sources_digest(&read_sources(Path::new(&test.snapshot))?)? != test.digest {
            return Err(conflict("immutable test snapshot was changed"));
        }
        drop(fence.take());
        let _: SessionSummary =
            decode(service::open_session(service, test.session_id.clone()).await)?;
        let host = service
            .live_session(&test.session_id)
            .await
            .ok_or_else(|| conflict("test host unavailable"))?;
        let before = host.resource_state().await.map_err(resource_error)?;
        if before.body["binding"] != test.readback["binding"] {
            return Err(conflict(
                "test session no longer matches immutable snapshot binding; create a fresh test",
            ));
        }
        let mut run = host
            .begin_prompt(prompt.clone())
            .await
            .map_err(|e| conflict(&format!("test execution refused: {e}")))?;
        let outcome = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let event =
                    run.events.recv().await.map_err(|_| {
                        conflict("test event stream lost; execution outcome unknown")
                    })?;
                if let Some(outcome) = run.tracker.observe(&event, false).outcome {
                    return Ok::<_, WorkspaceError>(outcome);
                }
            }
        })
        .await
        .map_err(|_| conflict("test execution deadline; outcome unknown, not passed"))??;
        let outcome =
            outcome.map_err(|e| conflict(&format!("test did not complete successfully: {e:?}")))?;
        if !matches!(outcome, rpi_extcore::run::RunOutcome::Completed) {
            return Err(conflict("test cancelled/not completed; no passed evidence"));
        }
        // Settlement completes the run, but PromptRun still owns its
        // admission lease until dropped. Release it before project CAS.
        drop(run);
        let after = host.resource_state().await.map_err(resource_error)?;
        if before.binding != after.binding || before.generation != after.generation {
            return Err(conflict("test binding/generation changed during execution"));
        }
        test.evidence = Some(
            json!({"version":1,"outcome":"completed","sessionId":test.session_id,"digest":test.digest,"binding":after.body["binding"],"generation":after.generation,"promptDigest":digest(prompt.as_bytes()),"settledAt":nonce()}),
        );
        // Settlement evidence is durable even if another host begins a
        // run before the subsequent source-read fence can be reacquired.
        save_store(&root.join("store.json"), state)?;
        *fence = Some(self.owner.authoring_admission(service, project).await?);
        // A model may have edited draft source. Detect and invalidate current facts, never overwrite it.
        LocalWorkspace::sync_authoring(root, state)?;
        Ok(())
    }
    fn record_evidence(&self, id: &str, test_id: &str) -> Result<(), WorkspaceError> {
        let state = &self.state;
        let draft = state
            .drafts
            .iter()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid("unknown draft"))?;
        let test = draft
            .tests
            .iter()
            .find(|t| t.id == test_id)
            .ok_or_else(|| invalid("unknown test"))?;
        if !test.evidence.as_ref().is_some_and(|e| {
            e["outcome"] == "completed"
                && e["digest"] == test.digest
                && e["sessionId"] == test.session_id
        }) {
            return Err(conflict(
                "No owner-observed successful run settlement for this test. Run an explicit real test; messages/schema alone are not evidence.",
            ));
        }
        Ok(())
    }
    fn publish(
        &mut self,
        id: &str,
        revision: u64,
        expected: String,
        test_id: String,
        version: String,
    ) -> Result<(), WorkspaceError> {
        let state = &mut self.state;
        let publication_catalog = &self.publication_catalog;
        let project = self.project;
        identifier(&version)?;
        let draft = state
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| invalid("unknown draft"))?;
        LocalWorkspace::check_revision(draft, revision, &expected)?;
        if draft.releases.iter().any(|r| r.version == version) {
            return Err(conflict(
                "immutable local version already published; choose a new version",
            ));
        }
        let test = draft
            .tests
            .iter()
            .find(|t| t.id == test_id && t.revision == revision && t.digest == expected)
            .ok_or_else(|| {
                conflict("publication requires a test of the exact current revision/digest")
            })?;
        if !test.evidence.as_ref().is_some_and(|e| {
            e["outcome"] == "completed"
                && e["digest"] == expected
                && e["sessionId"] == test.session_id
                && e["binding"] == test.readback["binding"]
        }) {
            return Err(conflict(
                "publication requires successful native run settlement tied to this snapshot binding",
            ));
        }
        if sources_digest(&read_sources(Path::new(&test.snapshot))?)? != expected {
            return Err(conflict("snapshot changed; refusing publication"));
        }
        let artifact = self
            .owner
            .inner
            .data
            .join("artifacts")
            .join(project)
            .join(id)
            .join(format!("release-{version}"));
        write_sources(&artifact, &draft.sources)?;
        let catalog = publication_catalog
            .as_ref()
            .ok_or_else(|| invalid("publication catalog unavailable"))?;
        let installed_reference = catalog.resources.iter().find(|r| r.facts["source"].as_str() == Some(&test.snapshot)).map(|r| r.reference.clone()).ok_or_else(|| conflict("tested installation unavailable; install immutable snapshot explicitly before publishing"))?;
        draft.releases.push(AuthoringRelease {
            version,
            digest: expected,
            test_id,
            source: artifact.to_string_lossy().into_owned(),
            installed_reference,
        });
        Ok(())
    }
}
