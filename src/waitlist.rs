use core::{mem::take, sync::atomic::*, task::Waker};
use crossbeam_utils::CachePadded;
use derivative::Derivative;
use smallvec::SmallVec;
use spinning_top::Spinlock;

pub(super) trait Wake {
    fn wake(self);
    fn will_wake(&self, other: &Self) -> bool;
}

impl Wake for Waker {
    fn wake(self) {
        self.wake()
    }

    fn will_wake(&self, other: &Self) -> bool {
        self.will_wake(other)
    }
}

#[derive(Derivative)]
#[derivative(Debug, Default(bound = "", new = "true"))]
pub(super) struct Waitlist<W> {
    #[derivative(Default(value = "AtomicBool::new(true)"))]
    empty: AtomicBool,
    wakers: CachePadded<Spinlock<SmallVec<[W; 6]>>>,
}

impl<W: Wake> Waitlist<W> {
    pub(super) fn wait(&self, waker: W) {
        let mut wakers = self.wakers.lock();
        if !wakers.iter().any(|w| w.will_wake(&waker)) {
            wakers.push(waker);
            drop(wakers); // release the lock
            self.empty.store(false, Ordering::Release);
        }
    }

    pub(super) fn wake(&self) {
        if !self.empty.swap(true, Ordering::Acquire) {
            // Drain all wakers in case any has become stale.
            let wakers = take(&mut *self.wakers.lock());
            // Important: do not inline `wakers` to ensure the lock is dropped.
            for waker in wakers {
                waker.wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockall::*;
    use rayon::scope;
    use test_strategy::proptest;

    mock! {
        Waker {}
        impl Wake for Waker {
            fn wake(self);
            fn will_wake(&self, other: &MockWaker) -> bool;
        }
    }

    #[test]
    fn waitlist_starts_empty() {
        let waitlist = Waitlist::<MockWaker>::new();
        assert!(waitlist.empty.load(Ordering::Relaxed));
        assert_eq!(waitlist.wakers.lock().len(), 0);
    }

    #[proptest]
    fn wait_stores_wakers(#[strategy(1..=100usize)] m: usize) {
        let waitlist = Waitlist::new();

        for i in 0..m {
            let mut waker = MockWaker::new();
            waker
                .expect_will_wake()
                .times(m - i - 1)
                .return_const(false);
            waitlist.wait(waker);
        }

        assert!(!waitlist.empty.load(Ordering::Relaxed));
        assert_eq!(waitlist.wakers.lock().len(), m);
    }

    #[proptest]
    fn wait_ignores_redundant_wakers(#[strategy(1..=100usize)] m: usize) {
        let waitlist = Waitlist::new();

        let mut waker = MockWaker::new();
        waker.expect_will_wake().times(m).return_const(true);
        waitlist.wait(waker);

        assert!(!waitlist.empty.load(Ordering::Relaxed));
        assert_eq!(waitlist.wakers.lock().len(), 1);

        for _ in 0..m {
            let mut waker = MockWaker::new();
            waker.expect_will_wake().never().return_const(false);
            waitlist.wait(waker);
        }

        assert!(!waitlist.empty.load(Ordering::Relaxed));
        assert_eq!(waitlist.wakers.lock().len(), 1);
    }

    #[proptest]
    fn wakers_are_woken_exactly_once(
        #[strategy(1..=100usize)] m: usize,
        #[strategy(1..=100usize)] n: usize,
    ) {
        let waitlist = Waitlist::new();

        for _ in 0..m {
            let mut waker = MockWaker::new();
            waker.expect_will_wake().return_const(false);
            waker.expect_wake().once().return_const(());
            waitlist.wait(waker);
        }

        for _ in 0..n {
            waitlist.wake();
        }
    }

    #[proptest]
    fn waitlist_is_thread_safe(
        #[strategy(1..=100usize)] m: usize,
        #[strategy(1..=100usize)] n: usize,
    ) {
        let waitlist = Waitlist::new();

        scope(|s| {
            for _ in 0..m {
                s.spawn(|_| {
                    let mut waker = MockWaker::new();
                    waker.expect_will_wake().return_const(false);
                    waker.expect_wake().times(0..=1).return_const(());
                    waitlist.wait(waker);
                });
            }

            for _ in 0..n {
                s.spawn(|_| {
                    waitlist.wake();
                });
            }
        });
    }
}
