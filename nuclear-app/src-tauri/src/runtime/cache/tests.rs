use super::*;

#[tokio::test]
async fn mutation_waits_for_readers_and_publishes_replacement() {
    let cache = RuntimeCache::new();
    let reader = cache
        .get_or_initialize(|| async { Ok(Some(7_u64)) })
        .await
        .unwrap()
        .unwrap();
    let acquire = cache.begin_mutation();
    tokio::pin!(acquire);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut acquire)
            .await
            .is_err()
    );
    drop(reader);
    let mutation = acquire.await;
    mutation.publish(Ok(Some(9)));
    assert_eq!(*cache.get_initialized().unwrap().unwrap(), 9);
}

#[tokio::test]
async fn dropped_mutation_requires_reinitialization() {
    let cache = RuntimeCache::new();
    cache
        .get_or_initialize(|| async { Ok(Some(7_u64)) })
        .await
        .unwrap();
    drop(cache.begin_mutation().await);
    assert!(cache.get_initialized().is_err());
    assert_eq!(
        *cache
            .get_or_initialize(|| async { Ok(Some(11_u64)) })
            .await
            .unwrap()
            .unwrap(),
        11
    );
}

#[tokio::test]
async fn dropped_waiter_does_not_abandon_owned_builder() {
    let cache = RuntimeCache::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let waiting_cache = cache.clone();
    let waiter = tokio::spawn(async move {
        waiting_cache
            .get_or_initialize(move || async move {
                started_tx.send(()).unwrap();
                finish_rx.await.unwrap();
                Ok(Some(13_u64))
            })
            .await
    });
    started_rx.await.unwrap();
    waiter.abort();

    let mutation = cache.begin_mutation();
    tokio::pin!(mutation);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut mutation)
            .await
            .is_err()
    );
    finish_tx.send(()).unwrap();
    let mutation = mutation.await;
    mutation.publish(Ok(Some(17)));
    assert_eq!(*cache.get_initialized().unwrap().unwrap(), 17);
}

#[tokio::test]
async fn panicking_builder_releases_building_for_retry() {
    let cache = RuntimeCache::<u64>::new();
    let first = cache
        .get_or_initialize(|| async {
            panic!("synthetic runtime verification panic");
        })
        .await
        .unwrap_err();
    assert!(first.contains("stopped before publishing"));
    assert_eq!(
        *cache
            .get_or_initialize(|| async { Ok(Some(23)) })
            .await
            .unwrap()
            .unwrap(),
        23
    );
}

#[tokio::test]
async fn mutation_wait_can_be_cancelled_without_changing_cache() {
    let cache = RuntimeCache::new();
    let reader = cache
        .get_or_initialize(|| async { Ok(Some(29_u64)) })
        .await
        .unwrap()
        .unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let wait = async {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            mutation = cache.begin_mutation() => Some(mutation),
        }
    };
    tokio::pin!(wait);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut wait)
            .await
            .is_err()
    );
    cancellation.cancel();
    assert!(wait.await.is_none());
    assert_eq!(*reader, 29);
    drop(reader);
    cache.begin_mutation().await.publish(Ok(Some(31)));
    assert_eq!(*cache.get_initialized().unwrap().unwrap(), 31);
}
