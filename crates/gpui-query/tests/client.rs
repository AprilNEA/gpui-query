use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::channel::oneshot;
use gpui::{AppContext as _, TestAppContext};
use gpui_query::{Query, QueryState};

struct QueryView {
    query: Query<i32, String>,
}

#[gpui::test]
async fn set_writes_local_data_without_fetching(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let calls = Arc::new(AtomicUsize::new(0));
    let view = cx.update(|cx| {
        cx.new(|cx| {
            let calls = Arc::clone(&calls);
            let query = Query::new(
                ("set",),
                move |_| {
                    let value = calls.fetch_add(1, Ordering::SeqCst) as i32 + 1;
                    async move { Ok(value) }
                },
                cx,
            );
            QueryView { query }
        })
    });
    cx.run_until_parked();

    query_client.set::<_, i32, String>(("set",), 42);
    cx.run_until_parked();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 42);
            assert!(!is_validating);
        }
        state => panic!("expected local write, got {state:?}"),
    });
}

#[gpui::test]
async fn successful_mutation_populates_then_revalidates(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = cx.executor();
    let view = cx.update(|cx| {
        cx.new(|cx| {
            let calls = Arc::clone(&calls);
            let executor = executor.clone();
            let query = Query::new(
                ("mutate-ok",),
                move |_| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    let executor = executor.clone();
                    async move {
                        if call > 0 {
                            executor.timer(Duration::from_secs(5)).await;
                        }
                        Ok(if call == 0 { 1 } else { 3 })
                    }
                },
                cx,
            );
            QueryView { query }
        })
    });
    cx.run_until_parked();

    let (settle_tx, settle_rx) = oneshot::channel::<i32>();
    let task = cx.update(|cx| {
        query_client.mutate(
            ("mutate-ok",),
            Some(9),
            async move {
                Ok::<Option<i32>, String>(Some(settle_rx.await.expect("mutation value")))
            },
            cx,
        )
    });
    cx.run_until_parked();
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready { data, .. } => assert_eq!(*data, 9),
        state => panic!("expected optimistic value, got {state:?}"),
    });

    settle_tx.send(2).expect("mutation receiver alive");
    let result = task.await.expect("mutation succeeds");
    assert_eq!(result.as_deref(), Some(&2));
    cx.run_until_parked();

    assert_eq!(calls.load(Ordering::SeqCst), 2, "settlement revalidates");
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 2, "mutation result populated before refresh");
            assert!(is_validating);
        }
        state => panic!("expected populated mutation value, got {state:?}"),
    });

    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 3);
            assert!(!is_validating);
        }
        state => panic!("expected authoritative refresh, got {state:?}"),
    });
}

#[gpui::test]
async fn failed_mutation_rolls_back_before_revalidation_settles(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = cx.executor();
    let view = cx.update(|cx| {
        cx.new(|cx| {
            let calls = Arc::clone(&calls);
            let executor = executor.clone();
            let query = Query::new(
                ("mutate-error",),
                move |_| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    let executor = executor.clone();
                    async move {
                        if call > 0 {
                            executor.timer(Duration::from_secs(5)).await;
                        }
                        Ok(if call == 0 { 1 } else { 2 })
                    }
                },
                cx,
            );
            QueryView { query }
        })
    });
    cx.run_until_parked();

    let (settle_tx, settle_rx) = oneshot::channel::<()>();
    let task = cx.update(|cx| {
        query_client.mutate(
            ("mutate-error",),
            Some(9),
            async move {
                settle_rx.await.expect("mutation signal");
                Err("rejected".to_string())
            },
            cx,
        )
    });
    cx.run_until_parked();
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready { data, .. } => assert_eq!(*data, 9),
        state => panic!("expected optimistic value, got {state:?}"),
    });

    settle_tx.send(()).expect("mutation receiver alive");
    let error = task.await.expect_err("mutation fails");
    assert_eq!(error.as_str(), "rejected");
    cx.run_until_parked();

    assert_eq!(calls.load(Ordering::SeqCst), 2, "failure revalidates");
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 1, "rollback restores the pre-optimistic snapshot");
            assert!(is_validating);
        }
        state => panic!("expected rolled-back value, got {state:?}"),
    });

    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 2);
            assert!(!is_validating);
        }
        state => panic!("expected settled revalidation, got {state:?}"),
    });
}
