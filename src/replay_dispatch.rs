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

pub(crate) struct ReplayPersistenceService {
    sender: Option<SyncSender<ReplayPersistRequest>>,
    worker: Option<JoinHandle<()>>,
}

impl ReplayPersistenceService {
    pub(crate) fn spawn() -> Self {
        let (sender, receiver) =
            sync_channel::<ReplayPersistRequest>(REPLAY_PERSIST_QUEUE_CAPACITY);
        let worker = std::thread::spawn(move || {
            let mut stores = HashMap::<std::path::PathBuf, replay::ReplayStore>::new();
            while let Ok(request) = receiver.recv() {
                // The worker owns the temp WAV for this request. Capture its path so
                // we can delete it after persistence (and thus after audio retention)
                // regardless of which branch persist_request returns through.
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

    /// Hand a replay persistence request to the background worker.
    ///
    /// Returns `true` when the worker has accepted the request and thereby taken
    /// ownership of the recorded temp WAV: the worker deletes it only after audio
    /// retention has had a chance to copy it. Returns `false` when replay is
    /// disabled or the request was dropped, in which case the caller remains
    /// responsible for deleting the WAV. This ordering prevents a race where the
    /// runtime deletes the temp WAV before the async worker can retain its audio.
    pub(crate) fn enqueue(
        &self,
        resolved: Option<ResolvedUtteranceConfig>,
        envelope: MuninnEnvelopeV1,
        outcome: PipelineOutcome,
        route: InjectionRoute,
        recorded: RecordedAudio,
    ) -> bool {
        let Some(resolved) = resolved else {
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
