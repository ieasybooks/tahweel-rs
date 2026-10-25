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
                        state.lanes[i].enabled && state.lanes[i].in_flight < self.per_account
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
}
