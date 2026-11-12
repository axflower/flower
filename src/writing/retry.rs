//! Opt-in retries with one monotonic budget, independent of transport.
use super::WritingError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(super) struct Runtime<N, S> {
    pub now: N,
    pub sleep: S,
    pub jitter: u32,
}

pub(super) fn run(
    allow: bool,
    cancel: &AtomicBool,
    deadline: Instant,
    mut generate: impl FnMut(&mut dyn FnMut(String)) -> Result<String, WritingError>,
    emit: &mut dyn FnMut(String),
    mut waiting: impl FnMut(u8, u32),
    runtime: Runtime<impl FnMut() -> Instant, impl FnMut(Duration)>,
) -> Result<String, WritingError> {
    let Runtime {
        mut now,
        mut sleep,
        jitter,
    } = runtime;
    let mut emitted = false;
    for attempt in 1..=3 {
        check(cancel, now(), deadline)?;
        let result = generate(&mut |chunk| {
            emitted |= !chunk.is_empty();
            emit(chunk);
        });
        check(cancel, now(), deadline)?;
        let seconds = match result {
            Err(WritingError::AccountRateLimited(s) | WritingError::ServiceBusy(s))
                if allow && !emitted && attempt < 3 =>
            {
                s
            }
            other => return other,
        };
        let delay = seconds.max(if attempt == 1 { 3 } else { 8 } + jitter.min(1));
        let current = now();
        let remaining = deadline.saturating_duration_since(current);
        let duration = Duration::from_secs(delay as u64);
        if duration >= remaining {
            return Err(WritingError::Timeout);
        }
        let until = current + duration;
        let mut shown = 0;
        loop {
            let current = now();
            check(cancel, current, deadline)?;
            if current >= until {
                break;
            }
            let left = until.duration_since(current);
            let seconds = left.as_secs() as u32 + u32::from(left.subsec_nanos() != 0);
            if seconds != shown {
                waiting(attempt + 1, seconds);
                shown = seconds;
            }
            sleep(left.min(Duration::from_millis(25)));
        }
    }
    unreachable!()
}

fn check(cancel: &AtomicBool, now: Instant, deadline: Instant) -> Result<(), WritingError> {
    if cancel.load(Ordering::Acquire) {
        Err(WritingError::Cancelled)
    } else if now >= deadline {
        Err(WritingError::Timeout)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    type ScenarioResult = (Result<String, WritingError>, u8, Vec<(u8, u32)>, Duration);

    fn scenario(
        error: WritingError,
        allow: bool,
        partial: bool,
        budget: u64,
        cancel_wait: bool,
    ) -> ScenarioResult {
        let start = Instant::now();
        let clock = Cell::new(start);
        let cancel = AtomicBool::new(false);
        let mut count = 0;
        let mut waits = Vec::new();
        let result = run(
            allow,
            &cancel,
            start + Duration::from_secs(budget),
            |emit| {
                count += 1;
                if partial {
                    emit("x".into());
                }
                Err(error)
            },
            &mut |_| {},
            |attempt, seconds| {
                waits.push((attempt, seconds));
                if cancel_wait {
                    cancel.store(true, Ordering::Release);
                }
            },
            Runtime {
                now: || clock.get(),
                sleep: |d| clock.set(clock.get() + d),
                jitter: 0,
            },
        );
        (result, count, waits, clock.get() - start)
    }

    #[test]
    fn exact_attempts_opt_in_known_only_and_partial() {
        for error in [
            WritingError::AccountRateLimited(0),
            WritingError::ServiceBusy(0),
        ] {
            let (result, count, waits, elapsed) = scenario(error, true, false, 90, false);
            assert_eq!(result, Err(error));
            assert_eq!(count, 3);
            assert!(waits.contains(&(2, 3)));
            assert!(waits.contains(&(3, 8)));
            assert_eq!(elapsed, Duration::from_secs(11));
            assert_eq!(scenario(error, false, false, 90, false).1, 1);
            assert_eq!(scenario(error, true, true, 90, false).1, 1);
        }
        for error in [
            WritingError::RateLimited,
            WritingError::Authentication,
            WritingError::Quota,
            WritingError::UsageLimit,
            WritingError::WrongKeyType,
            WritingError::Timeout,
            WritingError::Network,
        ] {
            assert_eq!(scenario(error, true, false, 90, false).1, 1);
        }
    }

    #[test]
    fn cancel_waiting_and_server_backoff_and_total_deadline() {
        let (result, count, _, elapsed) =
            scenario(WritingError::ServiceBusy(0), true, false, 90, true);
        assert_eq!(result, Err(WritingError::Cancelled));
        assert_eq!(count, 1);
        assert!(elapsed <= Duration::from_millis(25));
        let (_, count, waits, elapsed) =
            scenario(WritingError::ServiceBusy(12), true, false, 90, false);
        assert_eq!(count, 3);
        assert_eq!(waits[0], (2, 12));
        assert_eq!(elapsed, Duration::from_secs(24));
        let (result, count, _, _) =
            scenario(WritingError::AccountRateLimited(91), true, false, 90, false);
        assert_eq!(result, Err(WritingError::Timeout));
        assert_eq!(count, 1);
        let (result, count, _, elapsed) =
            scenario(WritingError::ServiceBusy(0), true, false, 10, false);
        assert_eq!(result, Err(WritingError::Timeout));
        assert_eq!(count, 2);
        assert_eq!(elapsed, Duration::from_secs(3));
    }

    #[test]
    fn success_after_one_retry_and_transport_time_share_budget() {
        let start = Instant::now();
        let clock = Cell::new(start);
        let cancel = AtomicBool::new(false);
        let mut count = 0;
        let result = run(
            true,
            &cancel,
            start + Duration::from_secs(90),
            |_| {
                count += 1;
                if count == 1 {
                    Err(WritingError::ServiceBusy(0))
                } else {
                    Ok("完成".into())
                }
            },
            &mut |_| {},
            |_, _| {},
            Runtime {
                now: || clock.get(),
                sleep: |d| clock.set(clock.get() + d),
                jitter: 1,
            },
        );
        assert_eq!(result, Ok("完成".into()));
        assert_eq!(count, 2);
        assert_eq!(clock.get() - start, Duration::from_secs(4));
        count = 0;
        clock.set(start);
        let result = run(
            true,
            &cancel,
            start + Duration::from_secs(90),
            |_| {
                count += 1;
                clock.set(clock.get() + Duration::from_secs(88));
                Err(WritingError::AccountRateLimited(0))
            },
            &mut |_| {},
            |_, _| {},
            Runtime {
                now: || clock.get(),
                sleep: |d| clock.set(clock.get() + d),
                jitter: 0,
            },
        );
        assert_eq!(result, Err(WritingError::Timeout));
        assert_eq!(count, 1);
        cancel.store(true, Ordering::Release);
        let result = run(
            true,
            &cancel,
            start + Duration::from_secs(90),
            |_| panic!("cancelled request must not start"),
            &mut |_| {},
            |_, _| {},
            Runtime {
                now: || start,
                sleep: |_| {},
                jitter: 0,
            },
        );
        assert_eq!(result, Err(WritingError::Cancelled));
    }
}
