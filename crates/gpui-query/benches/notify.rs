use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use gpui::{AppContext as _, Subscription, TestAppContext};
use gpui_query::Query;

const SUBSCRIBERS: usize = 1_000;
const SAMPLES: usize = 100;

struct QueryOwner {
    query: Query<usize, String>,
}

struct Subscriber {
    _subscription: Subscription,
}

fn main() {
    let mut cx = TestAppContext::single();
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let owner = cx.update(|cx| {
        cx.new(|cx| QueryOwner {
            query: Query::new(("notify-benchmark",), |_| std::future::ready(Ok(0)), cx),
        })
    });
    cx.run_until_parked();

    let mirror = cx.update(|cx| owner.read(cx).query.entity().clone());
    let callbacks = Arc::new(AtomicUsize::new(0));
    let subscribers = (0..SUBSCRIBERS)
        .map(|_| {
            let callbacks = Arc::clone(&callbacks);
            cx.update(|cx| {
                cx.new(|cx| Subscriber {
                    _subscription: cx.observe(&mirror, move |_, _, _| {
                        callbacks.fetch_add(1, Ordering::Relaxed);
                    }),
                })
            })
        })
        .collect::<Vec<_>>();

    let mut samples = Vec::with_capacity(SAMPLES);
    for value in 1..=SAMPLES {
        let start = Instant::now();
        query_client.set::<_, usize, String>(("notify-benchmark",), value);
        cx.run_until_parked();
        samples.push(start.elapsed());

        assert_eq!(
            callbacks.load(Ordering::Relaxed),
            value * SUBSCRIBERS,
            "every observer callback must complete before the sample ends"
        );
    }

    samples.sort_unstable();
    let median = (samples[SAMPLES / 2 - 1] + samples[SAMPLES / 2]) / 2;
    println!(
        "1000-subscriber notify: median {median:?} over {SAMPLES} samples ({} callbacks)",
        callbacks.load(Ordering::Relaxed)
    );

    // Keep the entities and their subscriptions alive through every sample.
    drop(subscribers);
    cx.quit();
}
