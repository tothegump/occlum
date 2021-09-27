use core::borrow::BorrowMut;
use core::task::Waker as RawWaker;
use futures::select_biased;

use atomic::{Atomic, Ordering};
use intrusive_collections::LinkedListLink;
use object_id::ObjectId;

use crate::prelude::*;
use crate::time::{TimerEntry, TimerFutureEntry, DURATION_ZERO};

/// A waiter.
///
/// `Waiter`s are mostly used with `WaiterQueue`s. Yet, it is also possible to
/// use `Waiter` with `Waker`.
pub struct Waiter {
    inner: Arc<WaiterInner>,
}

/// The states of a waiter.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WaiterState {
    Idle,
    Waiting,
    Woken,
}

impl Waiter {
    pub fn new() -> Self {
        let inner = Arc::new(WaiterInner::new());
        Self { inner }
    }

    pub fn state(&self) -> WaiterState {
        self.inner.state()
    }

    pub fn reset(&self) {
        self.inner.state.store(WaiterState::Idle, Ordering::Relaxed);
    }

    /// Wait until being woken by the waker.
    pub async fn wait(&self) {
        self.inner.wait().await;
    }

    /// Wait until being woken by the waker or reaching timeout.
    ///
    /// In each poll, we will first poll a `WaitFuture` object, if the result is `Ready`, return `Ok`.
    /// If the result is `Pending`, we will poll a `TimerEntry` object, return `Err` if got `Ready`.
    pub async fn wait_timeout<T: BorrowMut<Duration>>(
        &self,
        timeout: Option<&mut T>,
    ) -> Result<()> {
        match timeout {
            Some(t) => {
                let timer_entry = TimerEntry::new(*t.borrow_mut());
                select_biased! {
                    _ = self.inner.wait().fuse() => {
                        // We wake up before timeout expired.
                        *t.borrow_mut() = timer_entry.remained_duration();
                    }
                    _ = TimerFutureEntry::new(&timer_entry).fuse() => {
                        // The timer expired, we reached timeout.
                        *t.borrow_mut() = DURATION_ZERO;
                        return_errno!(ETIMEDOUT, "the waiter reached timeout");
                    }
                };
            }
            None => self.wait().await,
        };
        Ok(())
    }

    pub fn waker(&self) -> Waker {
        Waker {
            inner: self.inner.clone(),
        }
    }

    pub(super) fn inner(&self) -> &Arc<WaiterInner> {
        &self.inner
    }
}

#[derive(Clone)]
pub struct Waker {
    inner: Arc<WaiterInner>,
}

impl Waker {
    pub fn state(&self) -> WaiterState {
        self.inner.state()
    }

    pub fn wake(&self) -> Option<()> {
        self.inner.wake()
    }
}

// Accesible by WaiterQueue.
pub(super) struct WaiterInner {
    state: Atomic<WaiterState>,
    raw_waker: Mutex<Option<RawWaker>>,
    queue_id: Atomic<ObjectId>,
    pub(super) link: LinkedListLink,
}

impl WaiterInner {
    pub fn new() -> Self {
        Self {
            state: Atomic::new(WaiterState::Idle),
            link: LinkedListLink::new(),
            raw_waker: Mutex::new(None),
            queue_id: Atomic::new(ObjectId::null()),
        }
    }

    pub fn state(&self) -> WaiterState {
        self.state.load(Ordering::Relaxed)
    }

    pub fn set_state(&self, new_state: WaiterState) {
        self.state.store(new_state, Ordering::Relaxed)
    }

    pub fn queue_id(&self) -> &Atomic<ObjectId> {
        &self.queue_id
    }

    pub fn wait(&self) -> WaitFuture<'_> {
        WaitFuture::new(self)
    }

    pub fn wake(&self) -> Option<()> {
        let mut raw_waker = self.raw_waker.lock();
        match self.state() {
            WaiterState::Idle => {
                self.set_state(WaiterState::Woken);
                Some(())
            }
            WaiterState::Waiting => {
                self.set_state(WaiterState::Woken);

                let raw_waker = raw_waker.take().unwrap();
                raw_waker.wake();
                Some(())
            }
            WaiterState::Woken => None,
        }
    }
}

unsafe impl Sync for WaiterInner {}
unsafe impl Send for WaiterInner {}

pub struct WaitFuture<'a> {
    waiter: &'a WaiterInner,
}

impl<'a> WaitFuture<'a> {
    fn new(waiter: &'a WaiterInner) -> Self {
        Self { waiter }
    }
}

impl<'a> Future for WaitFuture<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut raw_waker = self.waiter.raw_waker.lock();
        match self.waiter.state() {
            WaiterState::Idle => {
                self.waiter.set_state(WaiterState::Waiting);

                *raw_waker = Some(cx.waker().clone());
                Poll::Pending
            }
            WaiterState::Waiting => {
                *raw_waker = Some(cx.waker().clone());
                Poll::Pending
            }
            WaiterState::Woken => {
                debug_assert!(raw_waker.is_none());
                Poll::Ready(())
            }
        }
    }
}

impl<'a> Drop for WaitFuture<'a> {
    fn drop(&mut self) {
        let mut raw_waker = self.waiter.raw_waker.lock();
        if let WaiterState::Waiting = self.waiter.state() {
            *raw_waker = None;
            self.waiter.set_state(WaiterState::Idle);
        }
    }
}