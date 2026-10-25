use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::{sync::Notify, time::Instant};

use crate::DriveError;

pub(crate) struct Scheduler {
    state: Mutex<State>,
    changed: Notify,
    per_account: usize,
}

struct State {
    lanes: Vec<Lane>,
    next: usize,
    credentials_rejected: bool,
}

struct Lane {
    in_flight: usize,
    limit: usize,
    successes: usize,
    held_until: Instant,
    enabled: bool,
}

impl Scheduler {
    pub(crate) fn new(accounts: usize, per_account: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                lanes: (0..accounts)
                    .map(|_| Lane {
                        in_flight: 0,
                        limit: per_account,
                        successes: 0,
                        held_until: Instant::now(),
                        enabled: true,
                    })
                    .collect(),
                next: 0,
                credentials_rejected: false,
            }),
            changed: Notify::new(),
            per_account,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Assign only when a lane can start now. Waiting callers can then use a different
    /// lane if it becomes available sooner, without reserving capacity on a held lane.
    pub(crate) async fn acquire(self: &Arc<Self>) -> Result<Slot, DriveError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let deadline = {
                let mut state = self.lock();
                if state.credentials_rejected {
                    return Err(DriveError::CredentialsRejected);
                }
                if state.lanes.iter().all(|lane| !lane.enabled) {
                    return Err(DriveError::NoAccounts);
                }
                let now = Instant::now();
                let count = state.lanes.len();
                let available = (0..count)
                    .map(|offset| (state.next + offset) % count)
                    .filter(|&i| {
                        state.lanes[i].enabled && state.lanes[i].in_flight < state.lanes[i].limit
                    });
                let ready = available
                    .clone()
                    .filter(|&i| state.lanes[i].held_until <= now)
                    .min_by_key(|&i| state.lanes[i].in_flight);
                if let Some(index) = ready {
                    state.lanes[index].in_flight += 1;
                    state.next = (index + 1) % count;
                    return Ok(Slot {
                        scheduler: self.clone(),
                        index,
                    });
                }
                available.map(|i| state.lanes[i].held_until).min()
            };
            if let Some(deadline) = deadline {
                tokio::select! {
                    () = &mut changed => {},
                    () = tokio::time::sleep_until(deadline) => {},
                }
            } else {
                changed.await;
            }
        }
    }

    pub(crate) fn enabled(&self, index: usize) -> bool {
        self.lock().lanes[index].enabled
    }

    /// Grow by one after `limit` consecutive successful HTTP attempts on this lane.
    pub(crate) fn succeeded(&self, index: usize) {
        let mut state = self.lock();
        let lane = &mut state.lanes[index];
        if lane.limit == self.per_account {
            return;
        }
        lane.successes += 1;
        if lane.successes < lane.limit {
            return;
        }
        let previous_limit = lane.limit;
        lane.limit += 1;
        lane.successes = 0;
        tracing::debug!(
            lane = index,
            previous_limit,
            limit = lane.limit,
            "raised Google Drive concurrency"
        );
        drop(state);
        self.changed.notify_waiters();
    }

    /// An overload halves only this lane's limit. Existing pages keep their slots;
    /// new pages wait until the in-flight count is below the reduced limit.
    pub(crate) fn failed(&self, index: usize, error: &DriveError) {
        let mut state = self.lock();
        let lane = &mut state.lanes[index];
        lane.successes = 0;
        if !error.is_overloaded() {
            return;
        }
        let previous_limit = lane.limit;
        lane.limit = (lane.limit / 2).max(1);
        if lane.limit == previous_limit {
            return;
        }
        tracing::debug!(
            lane = index,
            previous_limit,
            limit = lane.limit,
            "lowered Google Drive concurrency"
        );
        if lane.limit == 1 {
            tracing::warn!(
                lane = index,
                "Google Drive lane reduced to one page at a time"
            );
        }
    }

    pub(crate) fn disable(&self, index: usize) -> bool {
        let was_enabled = std::mem::replace(&mut self.lock().lanes[index].enabled, false);
        self.changed.notify_waiters();
        was_enabled
    }

    pub(crate) fn reject_credentials(&self) {
        self.lock().credentials_rejected = true;
        self.changed.notify_waiters();
    }

    pub(crate) fn hold(&self, index: usize, delay: Duration) -> Result<(), DriveError> {
        let until = Instant::now()
            .checked_add(delay)
            .ok_or(DriveError::InvalidResponse("retry delay is out of range"))?;
        let mut state = self.lock();
        state.lanes[index].held_until = state.lanes[index].held_until.max(until);
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    /// Existing uploaded files still need export/delete when their lane was disabled.
    pub(crate) async fn wait_hold(&self, index: usize) {
        loop {
            let until = self.lock().lanes[index].held_until;
            if until <= Instant::now() {
                return;
            }
            tokio::time::sleep_until(until).await;
        }
    }
}

/// Reserves an account until the page and its cleanup are finished.
pub(crate) struct Slot {
    scheduler: Arc<Scheduler>,
    pub(crate) index: usize,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.scheduler.lock().lanes[self.index].in_flight -= 1;
        self.scheduler.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_error(status: u16) -> DriveError {
        DriveError::Status {
            status,
            reasons: vec![],
            api_status: None,
            retry_after: None,
        }
    }

    async fn has_room(scheduler: &Arc<Scheduler>) -> bool {
        matches!(
            tokio::time::timeout(Duration::from_secs(1), scheduler.acquire()).await,
            Ok(Ok(_))
        )
    }

    async fn fill(scheduler: &Arc<Scheduler>, count: usize) -> Vec<Slot> {
        let mut slots = Vec::new();
        for _ in 0..count {
            slots.push(
                tokio::time::timeout(Duration::from_secs(1), scheduler.acquire())
                    .await
                    .expect("expected available capacity")
                    .unwrap(),
            );
        }
        assert!(!has_room(scheduler).await);
        slots
    }

    #[tokio::test(start_paused = true)]
    async fn caps_capacity_and_uses_available_lanes() {
        let scheduler = Scheduler::new(2, 1);
        let first = scheduler.acquire().await.unwrap();
        assert_eq!(first.index, 0);
        let second = scheduler.acquire().await.unwrap();
        assert_eq!(second.index, 1);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), scheduler.acquire())
                .await
                .is_err()
        );
        drop(second);
        assert_eq!(scheduler.acquire().await.unwrap().index, 1);
        drop(first);
        scheduler.hold(0, Duration::from_secs(10)).unwrap();
        assert_eq!(scheduler.acquire().await.unwrap().index, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn holds_extend_and_disabled_accounts_wake_waiters() {
        let scheduler = Scheduler::new(1, 1);
        scheduler.hold(0, Duration::from_secs(2)).unwrap();
        scheduler.hold(0, Duration::from_secs(3)).unwrap();
        let before = Instant::now();
        let slot = scheduler.acquire().await.unwrap();
        assert_eq!(before.elapsed(), Duration::from_secs(3));
        assert!(scheduler.disable(0));
        assert!(!scheduler.disable(0));
        assert!(matches!(
            scheduler.acquire().await,
            Err(DriveError::NoAccounts)
        ));
        drop(slot);
    }

    #[tokio::test(start_paused = true)]
    async fn overloads_halve_capacity_down_to_one() {
        for maximum in [1, 3, 8] {
            let scheduler = Scheduler::new(1, maximum);
            let mut expected = maximum;
            for status in [408, 500, 502, 503, 504] {
                scheduler.failed(0, &status_error(status));
                expected = (expected / 2).max(1);
                let _slots = fill(&scheduler, expected).await;
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn reducing_capacity_leaves_active_pages_running_and_waits_for_them() {
        let scheduler = Scheduler::new(1, 8);
        let mut slots = fill(&scheduler, 8).await;
        scheduler.failed(0, &status_error(503));
        assert_eq!(scheduler.lock().lanes[0].in_flight, 8);
        assert!(!has_room(&scheduler).await);
        slots.truncate(4);
        assert!(!has_room(&scheduler).await);
        slots.pop();
        let _next = scheduler.acquire().await.unwrap();
        assert!(!has_room(&scheduler).await);
    }

    #[tokio::test(start_paused = true)]
    async fn consecutive_successes_wake_waiters_and_recover_up_to_the_maximum() {
        let scheduler = Scheduler::new(1, 8);
        scheduler.failed(0, &status_error(503));
        let mut slots = fill(&scheduler, 4).await;
        for limit in 4..8 {
            let waiting_scheduler = scheduler.clone();
            let waiting = tokio::spawn(async move { waiting_scheduler.acquire().await.unwrap() });
            tokio::task::yield_now().await;
            for _ in 1..limit {
                scheduler.succeeded(0);
            }
            tokio::task::yield_now().await;
            assert!(!waiting.is_finished());
            scheduler.succeeded(0);
            slots.push(
                tokio::time::timeout(Duration::from_secs(1), waiting)
                    .await
                    .expect("growth must wake the waiting page")
                    .unwrap(),
            );
            assert!(!has_room(&scheduler).await);
        }
        for _ in 0..16 {
            scheduler.succeeded(0);
        }
        assert!(!has_room(&scheduler).await);
    }

    #[tokio::test(start_paused = true)]
    async fn failures_reset_success_runs_and_rate_limits_keep_their_holds() {
        for status in [400, 429] {
            let scheduler = Scheduler::new(1, 8);
            scheduler.failed(0, &status_error(503));
            let _slots = fill(&scheduler, 4).await;
            for _ in 0..3 {
                scheduler.succeeded(0);
            }
            scheduler.failed(0, &status_error(status));
            for _ in 0..3 {
                scheduler.succeeded(0);
            }
            assert!(!has_room(&scheduler).await);
            scheduler.hold(0, Duration::from_secs(5)).unwrap();
            scheduler.succeeded(0);
            let before = Instant::now();
            let _next = scheduler.acquire().await.unwrap();
            assert_eq!(before.elapsed(), Duration::from_secs(5));
            assert!(!has_room(&scheduler).await);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn lanes_reduce_and_recover_independently() {
        let scheduler = Scheduler::new(2, 4);
        scheduler.failed(0, &status_error(503));
        scheduler.failed(0, &status_error(503));
        let mut slots = fill(&scheduler, 5).await;
        assert_eq!(slots.iter().filter(|slot| slot.index == 0).count(), 1);
        assert_eq!(slots.iter().filter(|slot| slot.index == 1).count(), 4);
        scheduler.succeeded(1);
        assert!(!has_room(&scheduler).await);
        scheduler.succeeded(0);
        slots.push(scheduler.acquire().await.unwrap());
        assert_eq!(slots.last().unwrap().index, 0);
        assert!(!has_room(&scheduler).await);
        scheduler.failed(1, &status_error(503));
        drop(slots);
        let slots = fill(&scheduler, 4).await;
        assert_eq!(slots.iter().filter(|slot| slot.index == 0).count(), 2);
        assert_eq!(slots.iter().filter(|slot| slot.index == 1).count(), 2);
    }
}
