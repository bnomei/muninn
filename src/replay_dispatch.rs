//! Background worker that offloads replay persistence from the hot path.
//!
//! The runtime worker enqueues replay requests on a bounded sync channel; a
//! dedicated thread owns [`ReplayStore`] instances and deletes temporary WAV
//! files after persistence completes.

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::thread::JoinHandle;

use muninn::{
    InjectionRoute, MuninnEnvelopeV1, PipelineOutcome, RecordedAudio, ResolvedUtteranceConfig,
};
use tracing::{info, warn};

use crate::{logging, replay};

const REPLAY_PERSIST_QUEUE_CAPACITY: usize = 8;

struct ReplayPersistRequest {
    resolved: ResolvedUtteranceConfig,
    envelope: MuninnEnvelopeV1,
    outcome: PipelineOutcome,
    route: InjectionRoute,
    recorded: RecordedAudio,
}

/// Bounded-queue replay writer running on a background OS thread.
pub(crate) struct ReplayPersistenceService {
    sender: Option<SyncSender<ReplayPersistRequest>>,
    worker: Option<JoinHandle<()>>,
}

impl ReplayPersistenceService {
    /// Start the replay persistence worker thread.
    pub(crate) fn spawn() -> Self {
        let (sender, receiver) =
            sync_channel::<ReplayPersistRequest>(REPLAY_PERSIST_QUEUE_CAPACITY);
        let worker = std::thread::spawn(move || {
            let mut stores = HashMap::<std::path::PathBuf, replay::ReplayStore>::new();
            while let Ok(request) = receiver.recv() {
                let wav_path = request.recorded.wav_path.clone();
                persist_request(&mut stores, request);
                crate::runtime_pipeline::cleanup_recording_file(&wav_path);
            }
        });

        Self {
            sender: Some(sender),
            worker: Some(worker),
        }
    }

    /// Queue replay persistence for one utterance.
    ///
    /// Returns `true` when the worker accepted the request and owns temporary
    /// WAV cleanup. Returns `false` when replay is disabled, the queue is full,
    /// or the worker is unavailable; in those cases the caller retains cleanup.
    pub(crate) fn enqueue(
        &self,
        resolved: Option<ResolvedUtteranceConfig>,
        envelope: MuninnEnvelopeV1,
        outcome: PipelineOutcome,
        route: InjectionRoute,
        recorded: RecordedAudio,
    ) -> bool {
        let Some(resolved) =
            resolved.filter(|resolved| resolved.effective_config.logging.replay_enabled)
        else {
            return false;
        };
        let Some(sender) = self.sender.as_ref() else {
            warn!(
                target: logging::TARGET_RUNTIME,
                "dropping replay persistence request because service is shut down"
            );
            return false;
        };
        let request = ReplayPersistRequest {
            resolved,
            envelope,
            outcome,
            route,
            recorded,
        };
        match sender.try_send(request) {
            Ok(()) => true,
            Err(TrySendError::Full(_request)) => {
                warn!(
                    target: logging::TARGET_RUNTIME,
                    queue_capacity = REPLAY_PERSIST_QUEUE_CAPACITY,
                    "dropping replay persistence request because replay queue is full"
                );
                false
            }
            Err(TrySendError::Disconnected(_request)) => {
                warn!(
                    target: logging::TARGET_RUNTIME,
                    "dropping replay persistence request because replay worker is unavailable"
                );
                false
            }
        }
    }
}

fn persist_request(
    stores: &mut HashMap<std::path::PathBuf, replay::ReplayStore>,
    request: ReplayPersistRequest,
) {
    let spec = match replay::replay_store_spec(&request.resolved.effective_config) {
        Ok(spec) => spec,
        Err(error) => {
            warn!(
                target: logging::TARGET_RUNTIME,
                error = %error,
                "failed to resolve replay store configuration"
            );
            return;
        }
    };

    let store = match stores.entry(spec.root.clone()) {
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            entry.get_mut().update_limits(&spec);
            entry.into_mut()
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            match replay::ReplayStore::open(spec.clone()) {
                Ok(store) => entry.insert(store),
                Err(error) => {
                    warn!(
                        target: logging::TARGET_RUNTIME,
                        replay_root = %spec.root.display(),
                        error = %error,
                        "failed to initialize replay persistence store"
                    );
                    return;
                }
            }
        }
    };

    match store.persist(
        request.resolved,
        request.envelope,
        request.outcome,
        request.route,
        request.recorded,
    ) {
        Ok(Some(path)) => {
            info!(
                target: logging::TARGET_RUNTIME,
                replay_dir = %path.display(),
                "persisted replay artifact"
            );
        }
        Ok(None) => {}
        Err(error) => {
            warn!(
                target: logging::TARGET_RUNTIME,
                error = %error,
                "failed to persist replay artifact"
            );
        }
    }
}

impl Drop for ReplayPersistenceService {
    fn drop(&mut self) {
        let _ = self.sender.take();
        if let Some(worker) = self.worker.take() {
            if let Err(error) = worker.join() {
                warn!(
                    target: logging::TARGET_RUNTIME,
                    error = ?error,
                    "failed to join replay persistence worker"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muninn::{
        AppConfig, InjectionRouteReason, InjectionTarget, PipelineTraceEntry,
        ResolvedBuiltinStepConfig, ResolvedTranscriptionRoute, TargetContextSnapshot,
        TranscriptionRouteSource,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{sync_channel, TryRecvError};

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "muninn-replay-dispatch-test-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn sample_resolved(root: &Path) -> ResolvedUtteranceConfig {
        let mut effective_config = AppConfig::launchable_default();
        effective_config.logging.replay_enabled = true;
        effective_config.logging.replay_dir = root.to_path_buf();
        ResolvedUtteranceConfig {
            target_context: TargetContextSnapshot {
                bundle_id: Some("com.openai.codex".to_string()),
                app_name: Some("Codex".to_string()),
                window_title: Some("replay dispatch test".to_string()),
                captured_at: "2026-03-06T10:00:00Z".to_string(),
            },
            matched_rule_id: Some("codex".to_string()),
            profile_id: "default".to_string(),
            voice_id: Some("codex_focus".to_string()),
            voice_glyph: Some('C'),
            fallback_reason: None,
            transcription_route: ResolvedTranscriptionRoute {
                providers: Vec::new(),
                source: TranscriptionRouteSource::PipelineInferred,
            },
            builtin_steps: ResolvedBuiltinStepConfig::from_app_config(&effective_config),
            effective_config,
        }
    }

    fn sample_envelope() -> MuninnEnvelopeV1 {
        MuninnEnvelopeV1::new("utt-replay-dispatch", "2026-03-05T22:30:00Z")
            .with_audio(Some("/tmp/input.wav".to_string()), 1450)
            .with_transcript_raw_text("hello")
            .with_output_final_text("HELLO")
    }

    fn sample_outcome() -> PipelineOutcome {
        PipelineOutcome::Completed {
            envelope: sample_envelope(),
            trace: Vec::<PipelineTraceEntry>::new(),
        }
    }

    fn sample_route() -> InjectionRoute {
        InjectionRoute {
            target: InjectionTarget::OutputFinalText("HELLO".to_string()),
            reason: InjectionRouteReason::SelectedOutputFinalText,
            pipeline_stop_reason: None,
        }
    }

    fn recorded_wav(root: &Path, name: &str) -> RecordedAudio {
        let path = root.join(name);
        fs::write(&path, b"wav").expect("write test wav");
        RecordedAudio::new(path, 1450)
    }

    fn sample_request(root: &Path, wav_name: &str) -> ReplayPersistRequest {
        ReplayPersistRequest {
            resolved: sample_resolved(root),
            envelope: sample_envelope(),
            outcome: sample_outcome(),
            route: sample_route(),
            recorded: recorded_wav(root, wav_name),
        }
    }

    #[test]
    fn enqueue_returns_true_when_worker_accepts_wav_cleanup_ownership() {
        let root = temp_dir("accepted");
        let (sender, receiver) = sync_channel(1);
        let service = ReplayPersistenceService {
            sender: Some(sender),
            worker: None,
        };
        let recorded = recorded_wav(&root, "accepted.wav");
        let wav_path = recorded.wav_path.clone();

        let accepted = service.enqueue(
            Some(sample_resolved(&root)),
            sample_envelope(),
            sample_outcome(),
            sample_route(),
            recorded,
        );

        assert!(accepted);
        let queued = receiver.try_recv().expect("worker queue owns request");
        assert_eq!(queued.recorded.wav_path, wav_path);
        assert!(wav_path.exists());
    }

    #[test]
    fn enqueue_returns_false_and_caller_keeps_wav_cleanup_when_queue_is_full() {
        let root = temp_dir("full");
        let (sender, receiver) = sync_channel(1);
        sender
            .try_send(sample_request(&root, "already-queued.wav"))
            .expect("pre-fill queue");
        let service = ReplayPersistenceService {
            sender: Some(sender),
            worker: None,
        };
        let recorded = recorded_wav(&root, "rejected-full.wav");
        let wav_path = recorded.wav_path.clone();

        let accepted = service.enqueue(
            Some(sample_resolved(&root)),
            sample_envelope(),
            sample_outcome(),
            sample_route(),
            recorded,
        );

        assert!(!accepted);
        assert!(wav_path.exists(), "caller still owns rejected WAV cleanup");
        let queued = receiver
            .try_recv()
            .expect("pre-filled request remains queued");
        assert_eq!(queued.recorded.wav_path, root.join("already-queued.wav"));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn enqueue_returns_false_and_caller_keeps_wav_cleanup_when_worker_is_disconnected() {
        let root = temp_dir("disconnected");
        let (sender, receiver) = sync_channel(1);
        drop(receiver);
        let service = ReplayPersistenceService {
            sender: Some(sender),
            worker: None,
        };
        let recorded = recorded_wav(&root, "rejected-disconnected.wav");
        let wav_path = recorded.wav_path.clone();

        let accepted = service.enqueue(
            Some(sample_resolved(&root)),
            sample_envelope(),
            sample_outcome(),
            sample_route(),
            recorded,
        );

        assert!(!accepted);
        assert!(wav_path.exists(), "caller still owns rejected WAV cleanup");
    }

    #[test]
    fn enqueue_returns_false_and_caller_keeps_wav_cleanup_when_replay_is_disabled() {
        let root = temp_dir("disabled");
        let (sender, receiver) = sync_channel(1);
        let service = ReplayPersistenceService {
            sender: Some(sender),
            worker: None,
        };
        let mut resolved = sample_resolved(&root);
        resolved.effective_config.logging.replay_enabled = false;
        let recorded = recorded_wav(&root, "rejected-disabled.wav");
        let wav_path = recorded.wav_path.clone();

        let accepted = service.enqueue(
            Some(resolved),
            sample_envelope(),
            sample_outcome(),
            sample_route(),
            recorded,
        );

        assert!(!accepted);
        assert!(wav_path.exists(), "caller still owns rejected WAV cleanup");
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }
}
