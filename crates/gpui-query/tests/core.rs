use std::future::{ready, Ready};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, TestAppContext};
use gpui_query::{Query, QueryState, ReadPolicy, RetryPolicy};
use swr_core::FetchError;

struct QueryView {
    query: Query<i32, String>,
}

struct OptionalQueryView {
    query: Option<Query<i32, String>>,
}

fn counting_fetcher(
    calls: Arc<AtomicUsize>,
) -> impl Fn((&'static str,)) -> Ready<Result<i32, String>> + Send + Sync + 'static {
    move |_| ready(Ok(calls.fetch_add(1, Ordering::SeqCst) as i32 + 1))
}

#[gpui::test]
async fn virtual_clock_drives_retry_timer(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let view = cx.update(|cx| {
        cx.new(|cx| {
            let calls = Arc::clone(&calls);
            let query = Query::new(
                ("retry-clock",),
                move |_| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        match call {
                            0 => Ok(7),
                            1 => Err("temporary".to_string()),
                            _ => Ok(8),
                        }
                    }
                },
                cx,
            )
            .retry(RetryPolicy {
                interval: Duration::from_secs(1),
                max_retries: Some(1),
            });
            QueryView { query }
        })
    });

    cx.run_until_parked();
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready { data, .. } => assert_eq!(*data, 7),
        state => panic!("expected initial data, got {state:?}"),
    });

    cx.update(|cx| view.read(cx).query.refetch());
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "retry is waiting");
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 7);
            assert!(is_validating);
        }
        state => panic!("expected stale data during retry, got {state:?}"),
    });

    // RetryPolicy waits interval << 1 for the first retry.
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 8);
            assert!(!is_validating);
        }
        state => panic!("expected retried data, got {state:?}"),
    });
}

#[gpui::test]
async fn subscriptions_deduplicate_and_revalidate_once_stale(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let calls = Arc::new(AtomicUsize::new(0));

    let first = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(("shared",), counting_fetcher(Arc::clone(&calls)), cx),
        })
    });
    let second = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(("shared",), counting_fetcher(Arc::clone(&calls)), cx),
        })
    });
    cx.run_until_parked();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "two simultaneous views share one flight"
    );

    cx.executor().advance_clock(Duration::from_secs(1));
    let fresh_window_subscriber = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(("shared",), counting_fetcher(Arc::clone(&calls)), cx),
        })
    });
    cx.run_until_parked();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "default two-second stale window suppresses a new request"
    );

    cx.executor().advance_clock(Duration::from_secs(2));
    let stale_subscriber = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(("shared",), counting_fetcher(Arc::clone(&calls)), cx),
        })
    });
    cx.run_until_parked();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "subscribing to stale data starts one background revalidation"
    );
    for view in [&first, &second, &fresh_window_subscriber, &stale_subscriber] {
        cx.update(|cx| match view.read(cx).query.state(cx) {
            QueryState::Ready { data, .. } => assert_eq!(*data, 2),
            state => panic!("expected shared refreshed data, got {state:?}"),
        });
    }
}

#[gpui::test]
async fn refresh_interval_uses_the_virtual_clock(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let _view = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(("refresh",), counting_fetcher(Arc::clone(&calls)), cx)
                .refresh_interval(Duration::from_secs(3)),
        })
    });
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[gpui::test]
async fn all_queries_dropped_allows_gc(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let calls = Arc::new(AtomicUsize::new(0));
    let view = cx.update(|cx| {
        cx.new(|cx| OptionalQueryView {
            query: Some(
                Query::new(("gc",), counting_fetcher(Arc::clone(&calls)), cx)
                    .gc_time(Duration::from_secs(3)),
            ),
        })
    });
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let query = view.update(cx, |view, _| view.query.take());
    drop(query);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(4));
    cx.run_until_parked();

    let cached = query_client
        .inner()
        .fetch(
            ("gc",),
            counting_fetcher(Arc::clone(&calls)),
            ReadPolicy::CacheOnly,
        )
        .await;
    assert!(matches!(cached, Err(FetchError::Miss)));
}

#[gpui::test]
async fn late_response_from_discarded_flight_cannot_overwrite_newer_data(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = cx.executor();
    let view = cx.update(|cx| {
        cx.new(|cx| {
            let calls = Arc::clone(&calls);
            let executor = executor.clone();
            let query = Query::new(
                ("race",),
                move |_| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    let executor = executor.clone();
                    async move {
                        executor
                            .timer(Duration::from_secs(if call == 0 { 10 } else { 1 }))
                            .await;
                        Ok(call as i32 + 1)
                    }
                },
                cx,
            );
            QueryView { query }
        })
    });
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    query_client.set::<_, i32, String>(("race",), 0);
    cx.update(|cx| view.read(cx).query.refetch());
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready { data, .. } => assert_eq!(*data, 2),
        state => panic!("expected the newer fast response, got {state:?}"),
    });

    cx.executor().advance_clock(Duration::from_secs(9));
    cx.run_until_parked();
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready { data, .. } => assert_eq!(*data, 2),
        state => panic!("late discarded response changed state: {state:?}"),
    });
}

#[gpui::test]
async fn retry_exhaustion_preserves_stale_data_in_error_state(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let view = cx.update(|cx| {
        cx.new(|cx| {
            let calls = Arc::clone(&calls);
            let query = Query::new(
                ("retry-error",),
                move |_| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if call == 0 {
                            Ok(5)
                        } else {
                            Err("unavailable".to_string())
                        }
                    }
                },
                cx,
            )
            .retry(RetryPolicy {
                interval: Duration::from_secs(1),
                max_retries: Some(1),
            });
            QueryView { query }
        })
    });
    cx.run_until_parked();

    cx.update(|cx| view.read(cx).query.refetch());
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Error { error, stale_data } => {
            assert_eq!(error.as_str(), "unavailable");
            assert_eq!(stale_data.as_deref(), Some(&5));
        }
        state => panic!("expected exhausted retry error, got {state:?}"),
    });
}

#[gpui::test]
async fn keep_previous_data_controls_key_switch_loading_state(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let executor = cx.executor();
    let keep_view = cx.update(|cx| {
        cx.new(|cx| {
            let executor = executor.clone();
            let query = Query::new(
                ("keep", 1_u64),
                move |(_, id)| {
                    let executor = executor.clone();
                    async move {
                        if id == 2 {
                            executor.timer(Duration::from_secs(5)).await;
                        }
                        Ok(id as i32)
                    }
                },
                cx,
            )
            .keep_previous_data(true);
            QueryView { query }
        })
    });
    let plain_view = cx.update(|cx| {
        cx.new(|cx| {
            let executor = executor.clone();
            let query = Query::new(
                ("plain", 1_u64),
                move |(_, id)| {
                    let executor = executor.clone();
                    async move {
                        if id == 2 {
                            executor.timer(Duration::from_secs(5)).await;
                        }
                        Ok(id as i32)
                    }
                },
                cx,
            );
            QueryView { query }
        })
    });
    cx.run_until_parked();

    keep_view.update(cx, |view, _| view.query.set_key(("keep", 2_u64)));
    plain_view.update(cx, |view, _| view.query.set_key(("plain", 2_u64)));
    cx.run_until_parked();

    cx.update(|cx| match keep_view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 1);
            assert!(is_validating);
        }
        state => panic!("previous data should avoid loading, got {state:?}"),
    });
    cx.update(|cx| {
        assert!(matches!(
            plain_view.read(cx).query.state(cx),
            QueryState::Loading
        ));
    });

    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    for view in [&keep_view, &plain_view] {
        cx.update(|cx| match view.read(cx).query.state(cx) {
            QueryState::Ready {
                data,
                is_validating,
            } => {
                assert_eq!(*data, 2);
                assert!(!is_validating);
            }
            state => panic!("new key did not settle: {state:?}"),
        });
    }
}
