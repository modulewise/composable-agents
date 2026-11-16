//! A task's cancel signal an agent can wait on anywhere its work can stop.

use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use wit_bindgen::rt::async_support::{FutureReader, spawn_local};

/// The task's cancel signal an agent can wait on anywhere its work can stop.
#[derive(Clone)]
pub struct Cancel {
    state: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    canceled: bool,
    wakers: Vec<Waker>,
}

impl Cancel {
    /// Waits for `invoke`'s `cancel` in the background.
    pub fn new(cancel: FutureReader<()>) -> Self {
        let state = Rc::new(RefCell::new(State::default()));
        let waiter = state.clone();
        spawn_local(async move {
            cancel.await;
            let mut state = waiter.borrow_mut();
            state.canceled = true;
            for waker in state.wakers.drain(..) {
                waker.wake();
            }
        });
        Cancel { state }
    }

    pub fn is_canceled(&self) -> bool {
        self.state.borrow().canceled
    }

    /// Runs `work` until it ends or the task is canceled. `None` means
    /// canceled, and `work` was dropped, which cancels what it was doing.
    pub async fn until_canceled<T>(&self, work: impl Future<Output = T>) -> Option<T> {
        let mut work = pin!(work);
        poll_fn(|cx| {
            if self.poll_canceled(cx).is_ready() {
                return Poll::Ready(None);
            }
            work.as_mut().poll(cx).map(Some)
        })
        .await
    }

    // Ready once the task is canceled. Until then, `cx`'s waker is woken when
    // it is.
    fn poll_canceled(&self, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.state.borrow_mut();
        if state.canceled {
            return Poll::Ready(());
        }
        if !state.wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
            state.wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A `Cancel` that is canceled when `canceled` is set, for tests.
    fn cancel(canceled: bool) -> Cancel {
        Cancel {
            state: Rc::new(RefCell::new(State {
                canceled,
                wakers: Vec::new(),
            })),
        }
    }

    // Polls `future` once.
    fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
        let mut future = pin!(future);
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn work_runs_to_its_end_unless_the_task_is_canceled() {
        assert_eq!(
            poll_once(cancel(false).until_canceled(async { 1 })),
            Poll::Ready(Some(1))
        );
        assert_eq!(
            poll_once(cancel(true).until_canceled(async { 1 })),
            Poll::Ready(None)
        );
    }

    #[test]
    fn a_cancel_wakes_work_that_is_waiting() {
        let cancel = cancel(false);
        let pending = poll_once(cancel.until_canceled(std::future::pending::<()>()));
        assert_eq!(pending, Poll::Pending);
        assert_eq!(cancel.state.borrow().wakers.len(), 1);
        cancel.state.borrow_mut().canceled = true;
        assert_eq!(
            poll_once(cancel.until_canceled(std::future::pending::<()>())),
            Poll::Ready(None)
        );
    }
}
