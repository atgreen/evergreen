//! Condition notifications must belong to the waiters selected by the notifier.

use std::sync::{Arc, mpsc};
use std::time::Duration;
use egcl_rt::sync::{EgclCondVar, EgclMutex};

#[test]
fn condition_wait_releases_and_restores_all_recursive_acquisitions() {
    let mutex = Arc::new(EgclMutex::new(None, true));
    let condition = Arc::new(EgclCondVar::default());
    mutex.grab(true, None).unwrap();
    mutex.grab(true, None).unwrap();
    let worker = {
        let mutex = Arc::clone(&mutex);
        let condition = Arc::clone(&condition);
        std::thread::spawn(move || {
            let acquired = mutex.grab(true, Some(Duration::from_millis(200))).unwrap();
            if acquired {
                assert_eq!(condition.notify(1), 1);
                mutex.release().unwrap();
            }
            acquired
        })
    };
    let woke = condition
        .wait(&mutex, Some(Duration::from_millis(500)))
        .unwrap();
    let acquired = worker.join().unwrap();
    mutex.release().unwrap();
    mutex.release().unwrap();
    assert!(
        mutex.release().is_err(),
        "restore exactly the original depth"
    );
    assert!(acquired, "wait must fully release the recursive mutex");
    assert!(woke);
    mutex.grab(true, None).unwrap();
    mutex.grab(true, None).unwrap();
    assert!(!condition.wait(&mutex, Some(Duration::ZERO)).unwrap());
    mutex.release().unwrap();
    mutex.release().unwrap();
    assert!(mutex.release().is_err(), "timeout restores the depth too");
}

#[test]
fn notify_one_does_not_turn_another_waiters_timeout_into_success() {
    let mutex = Arc::new(EgclMutex::default());
    let condition = Arc::new(EgclCondVar::default());
    let (ready_tx, ready_rx) = mpsc::channel();
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let mutex = Arc::clone(&mutex);
            let condition = Arc::clone(&condition);
            let ready = ready_tx.clone();
            std::thread::spawn(move || {
                assert!(mutex.grab(true, None).unwrap());
                ready.send(()).unwrap();
                let notified = condition
                    .wait(&mutex, Some(Duration::from_millis(500)))
                    .unwrap();
                mutex.release().unwrap();
                notified
            })
        })
        .collect();
    for _ in 0..2 {
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    // Every ready message is sent while holding this mutex. Acquiring it after
    // both messages proves both workers enqueued and released it in WAIT.
    assert!(mutex.grab(true, None).unwrap());
    condition.notify(1);
    mutex.release().unwrap();
    let notified = workers
        .into_iter()
        .map(|worker| usize::from(worker.join().unwrap()))
        .sum::<usize>();
    assert_eq!(notified, 1, "only the selected waiter was notified");
}

#[test]
fn notifications_count_selected_waiters_and_do_not_accumulate() {
    let mutex = Arc::new(EgclMutex::default());
    let condition = Arc::new(EgclCondVar::default());
    assert_eq!(condition.notify(usize::MAX), 0);
    assert_eq!(condition.broadcast(), 0);
    let (ready_tx, ready_rx) = mpsc::channel();
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let mutex = Arc::clone(&mutex);
            let condition = Arc::clone(&condition);
            let ready = ready_tx.clone();
            std::thread::spawn(move || {
                assert!(mutex.grab(true, None).unwrap());
                ready.send(()).unwrap();
                let notified = condition
                    .wait(&mutex, Some(Duration::from_secs(2)))
                    .unwrap();
                mutex.release().unwrap();
                notified
            })
        })
        .collect();
    for _ in 0..3 {
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    assert!(mutex.grab(true, None).unwrap());
    assert_eq!(condition.notify(0), 0);
    assert_eq!(condition.notify(1), 1);
    assert_eq!(condition.broadcast(), 2);
    assert_eq!(condition.broadcast(), 0);
    assert_eq!(condition.notify(usize::MAX), 0);
    mutex.release().unwrap();
    for worker in workers {
        assert!(worker.join().unwrap());
    }
    assert!(mutex.grab(true, None).unwrap());
    assert!(!condition.wait(&mutex, Some(Duration::ZERO)).unwrap());
    // A timed-out waiter is no longer eligible for notification; WAIT still
    // reacquires the mutex, as demonstrated by this release succeeding.
    assert_eq!(condition.notify(1), 0);
    mutex.release().unwrap();
}
