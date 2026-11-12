//! One owned background generation job, with a bounded latest-snapshot mailbox.

use super::{WritingError, WritingRequest, MAX_OUTPUT_CHARS};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static ACTIVE: AtomicBool = AtomicBool::new(false);
struct Lease(&'static AtomicBool);
impl Lease {
    fn acquire(gate: &'static AtomicBool) -> Result<Self, WritingError> {
        gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| WritingError::Busy)?;
        Ok(Self(gate))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Generating,
    Waiting { attempt: u8, seconds: u32 },
    Complete,
    Failed(WritingError),
}

#[derive(Clone)]
pub struct Update {
    pub id: u64,
    pub text: String,
    pub phase: Phase,
}

impl std::fmt::Debug for Update {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Update")
            .field("id", &self.id)
            .field("text", &"[REDACTED]")
            .field("phase", &self.phase)
            .finish()
    }
}

pub struct WritingJob {
    cancel: Arc<AtomicBool>,
    mailbox: Arc<Mutex<Option<Update>>>,
    thread: Option<JoinHandle<()>>,
}

fn publish(slot: &Mutex<Option<Update>>, cancel: &AtomicBool, mut update: Update) {
    if let Ok(mut current) = slot.lock() {
        // The decision and publication share cancel()'s mailbox lock. An
        // earlier atomic check alone would leave a check-to-publish race.
        if cancel.load(Ordering::Acquire) {
            update.phase = Phase::Failed(WritingError::Cancelled);
        }
        *current = Some(update);
    }
}

impl WritingJob {
    pub fn start(request: WritingRequest) -> Result<Self, WritingError> {
        Self::start_with_retry(request, false)
    }

    pub fn start_with_retry(
        request: WritingRequest,
        allow_retry: bool,
    ) -> Result<Self, WritingError> {
        Self::start_with_profile(request, super::config::ApiProfile::default(), allow_retry)
    }

    pub fn start_with_profile(
        request: WritingRequest,
        profile: super::config::ApiProfile,
        allow_retry: bool,
    ) -> Result<Self, WritingError> {
        request.validate()?;
        let profile = profile.normalized()?;
        // Reject before acquiring the generation lease or reading credentials.
        if profile.is_deepseek() && profile.protocol == super::config::ApiProtocol::Responses {
            return Err(WritingError::UnsupportedProtocol);
        }
        let allow_retry = allow_retry && profile.is_bigmodel();
        let lease = Lease::acquire(&ACTIVE)?;
        Self::spawn_inner(
            request,
            Some(lease),
            move |request, cancel, emit, waiting| {
                let deadline = Instant::now() + Duration::from_secs(super::REQUEST_TIMEOUT_SECS);
                #[cfg(windows)]
                {
                    let key = super::credentials::load_key_for(&profile)?
                        .ok_or(WritingError::MissingKey)?;
                    if cancel.load(Ordering::Acquire) {
                        return Err(WritingError::Cancelled);
                    }
                    let jitter = Instant::now()
                        .duration_since(deadline - Duration::from_secs(super::REQUEST_TIMEOUT_SECS))
                        .subsec_nanos()
                        & 1;
                    super::retry::run(
                        allow_retry,
                        cancel,
                        deadline,
                        |emit| {
                            super::transport::generate_until_for(
                                request, &profile, &key, cancel, emit, deadline,
                            )
                        },
                        emit,
                        waiting,
                        super::retry::Runtime {
                            now: Instant::now,
                            sleep: std::thread::sleep,
                            jitter,
                        },
                    )
                }
                #[cfg(not(windows))]
                {
                    let _ = (request, cancel, emit, waiting, allow_retry, deadline);
                    Err(WritingError::Unsupported)
                }
            },
        )
    }

    #[cfg(test)]
    fn spawn<F>(request: WritingRequest, generate: F) -> Result<Self, WritingError>
    where
        F: FnOnce(
                &WritingRequest,
                &AtomicBool,
                &mut dyn FnMut(String),
            ) -> Result<String, WritingError>
            + Send
            + 'static,
    {
        Self::spawn_inner(request, None, move |request, cancel, emit, _| {
            generate(request, cancel, emit)
        })
    }

    fn spawn_inner<F>(
        request: WritingRequest,
        lease: Option<Lease>,
        generate: F,
    ) -> Result<Self, WritingError>
    where
        F: FnOnce(
                &WritingRequest,
                &AtomicBool,
                &mut dyn FnMut(String),
                &mut dyn FnMut(u8, u32),
            ) -> Result<String, WritingError>
            + Send
            + 'static,
    {
        request.validate()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let mailbox = Arc::new(Mutex::new(Some(Update {
            id: request.id,
            text: String::new(),
            phase: Phase::Connecting,
        })));
        let (token, slot) = (cancel.clone(), mailbox.clone());
        let thread = std::thread::Builder::new()
            .name("flower-writing".into())
            .spawn(move || {
                let _lease = lease;
                let mut partial = String::new();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    generate(
                        &request,
                        &token,
                        &mut |chunk| {
                            if !token.load(Ordering::Acquire)
                                && partial.chars().count() + chunk.chars().count()
                                    <= MAX_OUTPUT_CHARS
                            {
                                partial.push_str(&chunk);
                                publish(
                                    &slot,
                                    &token,
                                    Update {
                                        id: request.id,
                                        text: partial.clone(),
                                        phase: Phase::Generating,
                                    },
                                );
                            }
                        },
                        &mut |attempt, seconds| {
                            publish(
                                &slot,
                                &token,
                                Update {
                                    id: request.id,
                                    text: String::new(),
                                    phase: Phase::Waiting { attempt, seconds },
                                },
                            )
                        },
                    )
                }))
                .unwrap_or(Err(WritingError::Service));
                let (text, phase) = if token.load(Ordering::Acquire) {
                    (partial, Phase::Failed(WritingError::Cancelled))
                } else {
                    match result {
                        Ok(text)
                            if !text.trim().is_empty()
                                && text.chars().count() <= MAX_OUTPUT_CHARS =>
                        {
                            (text, Phase::Complete)
                        }
                        Ok(_) => (partial, Phase::Failed(WritingError::Incomplete)),
                        Err(error) => (partial, Phase::Failed(error)),
                    }
                };
                publish(
                    &slot,
                    &token,
                    Update {
                        id: request.id,
                        text,
                        phase,
                    },
                );
            })
            .map_err(|_| WritingError::Service)?;
        Ok(Self {
            cancel,
            mailbox,
            thread: Some(thread),
        })
    }

    /// Nonblocking UI polling; no disk, network, credential or host COM access.
    pub fn poll(&self) -> Option<Update> {
        let mut current = self.mailbox.try_lock().ok()?;
        let mut update = current.take()?;
        if self.cancel.load(Ordering::Acquire) {
            update.phase = Phase::Failed(WritingError::Cancelled);
        }
        Some(update)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        // Invalidate already published but unconsumed success as well. Once
        // this returns, neither a pending publisher nor poll can return success.
        if let Ok(mut current) = self.mailbox.lock() {
            if let Some(update) = current.as_mut() {
                update.phase = Phase::Failed(WritingError::Cancelled);
            }
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .map_or(true, |thread| thread.is_finished())
    }
}

impl Drop for WritingJob {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            // Transport waits for request close callbacks before returning.
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn deepseek_responses_rejected_without_starting_job_or_loading_key() {
        let profile = super::super::config::ApiProfile {
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            protocol: super::super::config::ApiProtocol::Responses,
            ..Default::default()
        };
        assert!(matches!(
            WritingJob::start_with_profile(request(1), profile, true),
            Err(WritingError::UnsupportedProtocol)
        ));
    }

    #[test]
    fn invalid_profile_is_rejected_before_job_or_credential_access() {
        let profile = super::super::config::ApiProfile {
            base_url: "http://example.com".into(),
            ..Default::default()
        };
        assert!(matches!(
            WritingJob::start_with_profile(request(1), profile, true),
            Err(WritingError::InvalidConfiguration)
        ));
    }

    #[test]
    fn isolated_gate_rejects_concurrent_job_and_releases_after_cleanup() {
        static GATE: AtomicBool = AtomicBool::new(false);
        let lease = Lease::acquire(&GATE).unwrap();
        let (started, ready) = std::sync::mpsc::channel();
        let (release, cleanup) = std::sync::mpsc::channel();
        let job = WritingJob::spawn_inner(request(1), Some(lease), move |_, _, _, _| {
            started.send(()).unwrap();
            cleanup.recv().unwrap(); // Simulates transport close acknowledgement.
            Err(WritingError::Cancelled)
        })
        .unwrap();
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(matches!(Lease::acquire(&GATE), Err(WritingError::Busy)));
        job.cancel();
        assert!(matches!(Lease::acquire(&GATE), Err(WritingError::Busy)));
        release.send(()).unwrap();
        drop(job);
        drop(Lease::acquire(&GATE).unwrap());
        let lease = Lease::acquire(&GATE).unwrap();
        let job = WritingJob::spawn_inner(request(2), Some(lease), |_, _, _, _| panic!("fixture"))
            .unwrap();
        assert_eq!(wait(&job).phase, Phase::Failed(WritingError::Service));
        drop(job);
        assert!(Lease::acquire(&GATE).is_ok());
        let mut invalid = request(3);
        invalid.id = 0;
        let failed = WritingJob::spawn_inner(
            invalid,
            Some(Lease::acquire(&GATE).unwrap()),
            |_, _, _, _| unreachable!(),
        );
        assert!(matches!(failed, Err(WritingError::InvalidInput)));
        assert!(Lease::acquire(&GATE).is_ok());
    }

    fn request(id: u64) -> WritingRequest {
        WritingRequest {
            id,
            task: super::super::Task::draft(),
            script: crate::ChineseScript::Simplified,
            input: "test".into(),
        }
    }
    fn wait(job: &WritingJob) -> Update {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(update) = job.poll() {
                if matches!(update.phase, Phase::Complete | Phase::Failed(_)) {
                    return update;
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn bounded_mailbox_publishes_identity_and_complete_result() {
        let job = WritingJob::spawn(request(7), |_, _, emit| {
            for _ in 0..800 {
                emit("花".into());
            }
            Ok("花".repeat(800))
        })
        .unwrap();
        let update = wait(&job);
        assert_eq!(update.id, 7);
        assert_eq!(update.text.chars().count(), 800);
        assert_eq!(update.phase, Phase::Complete);
    }

    #[test]
    fn cancellation_wins_over_late_success_and_joins() {
        let job = WritingJob::spawn(request(1), |_, cancel, emit| {
            while !cancel.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            emit("late".into());
            Ok("late".into())
        })
        .unwrap();
        job.cancel();
        let update = wait(&job);
        assert_eq!(update.phase, Phase::Failed(WritingError::Cancelled));
        assert!(update.text.is_empty());
        drop(job);
    }

    #[test]
    fn partial_failure_is_not_a_completed_result() {
        let job = WritingJob::spawn(request(2), |_, _, emit| {
            emit("未完成".into());
            Err(WritingError::Network)
        })
        .unwrap();
        let update = wait(&job);
        assert_eq!(update.text, "未完成");
        assert_eq!(update.phase, Phase::Failed(WritingError::Network));
    }

    #[test]
    fn cancellation_invalidates_already_published_success() {
        let job = WritingJob::spawn(request(3), |_, _, _| Ok("完成".into())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !job.is_finished() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        job.cancel();
        assert_eq!(
            job.poll().unwrap().phase,
            Phase::Failed(WritingError::Cancelled)
        );
    }

    #[test]
    fn cancel_between_prepared_result_and_mailbox_publication_wins() {
        let slot = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let lock = slot.lock().unwrap();
        let (ready, prepared) = std::sync::mpsc::channel();
        let (worker_slot, worker_cancel) = (slot.clone(), cancel.clone());
        let publisher = std::thread::spawn(move || {
            let success = Update {
                id: 9,
                text: "完成".into(),
                phase: Phase::Complete,
            };
            ready.send(()).unwrap();
            publish(&worker_slot, &worker_cancel, success);
        });
        // The success value is already prepared, but publication is blocked.
        prepared.recv_timeout(Duration::from_secs(3)).unwrap();
        cancel.store(true, Ordering::Release);
        drop(lock);
        publisher.join().unwrap();
        assert_eq!(
            slot.lock().unwrap().as_ref().unwrap().phase,
            Phase::Failed(WritingError::Cancelled)
        );
    }
}
