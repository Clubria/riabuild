//! Driving several futures at once, and answering in the order they were given.
//!
//! Written out rather than pulled in. `futures-util` would supply `join_all`
//! and nothing else this workspace wants, and the same argument `filelock`
//! makes for `std::fs::File::try_lock` over `fs2` applies here: no dependency,
//! no `unsafe`, and the whole of it fits on one screen.
//!
//! What it is not is a thread pool. Every future here runs on riabuild's single
//! reactor thread — `main` is `#[tokio::main(flavor = "current_thread")]` — so
//! what overlaps is *waiting*: four downloads with their sockets open at once,
//! which is where a cold run spends its time. Work that does not yield still
//! holds the thread, so a task doing something expensive between awaits stalls
//! every task beside it. That is a reason to keep such work off the reactor,
//! not a reason to want threads here.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

/// Runs every future concurrently, and returns their outputs in the order the
/// futures were given — never the order they finished in.
///
/// The ordering is the point rather than a convenience: it is what lets the
/// engine report a concurrent wave in exactly the sequence a sequential run
/// reported it, so the transcript a developer reads (and the one the end-to-end
/// suites assert on) does not depend on which download happened to be quickest.
///
/// Every live future is polled on every wake. That is `join_all`'s own
/// behaviour and it is quadratic in the number of futures, which is worth
/// saying out loud and not worth fixing: the widest wave riabuild has is six.
pub(super) async fn join_in_order<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut running: Vec<Option<Pin<Box<F>>>> =
        futures.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut finished: Vec<Option<F::Output>> = running.iter().map(|_| None).collect();

    std::future::poll_fn(|cx| {
        let mut all_done = true;
        for (slot, output) in running.iter_mut().zip(finished.iter_mut()) {
            // `None` is a future that has already finished. Polling one again
            // is what the `Option` exists to prevent — it is a panic for most
            // futures, and this loop revisits every slot on every wake.
            let Some(future) = slot else { continue };
            match future.as_mut().poll(cx) {
                Poll::Ready(value) => {
                    *output = Some(value);
                    *slot = None;
                }
                Poll::Pending => all_done = false,
            }
        }
        if all_done {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;

    debug_assert!(
        finished.iter().all(Option::is_some),
        "join_in_order returned before every future finished"
    );
    finished.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// The property everything else here depends on: what comes back is in the
    /// order the futures were given, not the order they completed.
    #[tokio::test]
    async fn results_come_back_in_the_order_they_were_given() {
        // Descending sleeps, so completion order is the reverse of the input.
        let futures: Vec<_> = [30u64, 20, 10]
            .into_iter()
            .map(|ms| async move {
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                ms
            })
            .collect();

        assert_eq!(join_in_order(futures).await, vec![30, 20, 10]);
    }

    /// And that they really did overlap rather than take turns: three futures
    /// that each wait for the two others to have started cannot all finish
    /// unless all three were running at once.
    #[tokio::test]
    async fn the_futures_actually_run_at_the_same_time() {
        let started = RefCell::new(0u32);
        let futures: Vec<_> = (0..3)
            .map(|index| {
                let started = &started;
                async move {
                    *started.borrow_mut() += 1;
                    while *started.borrow() < 3 {
                        tokio::task::yield_now().await;
                    }
                    index
                }
            })
            .collect();

        // Sequential execution deadlocks here; concurrent execution does not.
        let order = tokio::time::timeout(std::time::Duration::from_secs(5), join_in_order(futures))
            .await
            .expect("three overlapping futures must finish");

        assert_eq!(order, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn nothing_at_all_is_not_a_hang() {
        let empty: Vec<std::future::Ready<u8>> = Vec::new();
        assert!(join_in_order(empty).await.is_empty());
    }
}
