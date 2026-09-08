use futures_util::FutureExt;
use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

enum CachePhase<T> {
    Uninitialized,
    Building,
    Ready(Result<Option<Arc<T>>, Arc<str>>),
    Mutating,
}

struct CacheState<T> {
    phase: CachePhase<T>,
    readers: usize,
}

struct CacheInner<T> {
    state: Mutex<CacheState<T>>,
    async_changed: Notify,
}

pub(super) struct RuntimeCache<T> {
    inner: Arc<CacheInner<T>>,
}

impl<T> Clone for RuntimeCache<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: Send + Sync + 'static> RuntimeCache<T> {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(CacheInner {
                state: Mutex::new(CacheState {
                    phase: CachePhase::Uninitialized,
                    readers: 0,
                }),
                async_changed: Notify::new(),
            }),
        }
    }

    pub(super) async fn get_or_initialize<F, Fut>(
        &self,
        builder: F,
    ) -> Result<Option<RuntimeCacheRead<T>>, String>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<T>, String>> + Send + 'static,
    {
        let mut builder = Some(builder);
        loop {
            let notified = self.inner.async_changed.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();
            let should_build = {
                let mut state = self.lock_state();
                match &state.phase {
                    CachePhase::Ready(value) => {
                        let value = value.clone();
                        return self.read_ready_value(&mut state, value);
                    }
                    CachePhase::Uninitialized => {
                        if builder.is_none() {
                            return Err(
                                "Runtime verification stopped before publishing its result.".into(),
                            );
                        }
                        state.phase = CachePhase::Building;
                        true
                    }
                    CachePhase::Building | CachePhase::Mutating => false,
                }
            };
            if should_build {
                let builder = builder
                    .take()
                    .expect("runtime cache builder is consumed once");
                let cache = self.clone();
                drop(tokio::spawn(async move {
                    let completion = RuntimeCacheBuildCompletion::new(cache);
                    if let Ok(result) = AssertUnwindSafe(builder()).catch_unwind().await {
                        completion.publish(result);
                    }
                }));
            }
            notified.await;
        }
    }

    pub(super) fn get_initialized(&self) -> Result<Option<RuntimeCacheRead<T>>, String> {
        let mut state = self.lock_state();
        match &state.phase {
            CachePhase::Ready(value) => {
                let value = value.clone();
                self.read_ready_value(&mut state, value)
            }
            CachePhase::Uninitialized => {
                Err("Downloader runtime verification is not initialized.".into())
            }
            CachePhase::Building | CachePhase::Mutating => {
                Err("Downloader runtime verification is temporarily unavailable.".into())
            }
        }
    }

    pub(super) async fn begin_mutation(&self) -> RuntimeCacheMutation<T> {
        loop {
            let notified = self.inner.async_changed.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();
            let acquired = {
                let mut state = self.lock_state();
                if matches!(state.phase, CachePhase::Building | CachePhase::Mutating)
                    || state.readers != 0
                {
                    false
                } else {
                    state.phase = CachePhase::Mutating;
                    true
                }
            };
            if acquired {
                return RuntimeCacheMutation {
                    inner: Arc::clone(&self.inner),
                    active: true,
                };
            }
            notified.await;
        }
    }

    fn read_ready_value(
        &self,
        state: &mut MutexGuard<'_, CacheState<T>>,
        value: Result<Option<Arc<T>>, Arc<str>>,
    ) -> Result<Option<RuntimeCacheRead<T>>, String> {
        match value {
            Ok(Some(snapshot)) => {
                state.readers = state.readers.saturating_add(1);
                Ok(Some(RuntimeCacheRead {
                    snapshot,
                    _permit: RuntimeCacheReadPermit {
                        inner: Arc::clone(&self.inner),
                    },
                }))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn complete_build(&self, result: Result<Option<T>, String>) {
        let mut state = self.lock_state();
        if !matches!(state.phase, CachePhase::Building) {
            return;
        }
        state.phase = CachePhase::Ready(match result {
            Ok(snapshot) => Ok(snapshot.map(Arc::new)),
            Err(error) => Err(Arc::from(error)),
        });
        drop(state);
        self.notify_all();
    }

    fn lock_state(&self) -> MutexGuard<'_, CacheState<T>> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn notify_all(&self) {
        self.inner.async_changed.notify_waiters();
    }
}

struct RuntimeCacheBuildCompletion<T> {
    cache: RuntimeCache<T>,
    active: bool,
}

impl<T: Send + Sync + 'static> RuntimeCacheBuildCompletion<T> {
    fn new(cache: RuntimeCache<T>) -> Self {
        Self {
            cache,
            active: true,
        }
    }

    fn publish(mut self, result: Result<Option<T>, String>) {
        self.cache.complete_build(result);
        self.active = false;
    }
}

impl<T> Drop for RuntimeCacheBuildCompletion<T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .cache
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(state.phase, CachePhase::Building) {
            state.phase = CachePhase::Uninitialized;
        }
        drop(state);
        self.cache.inner.async_changed.notify_waiters();
    }
}

pub(super) struct RuntimeCacheRead<T> {
    snapshot: Arc<T>,
    // Fields drop in declaration order. The snapshot must release its verified
    // file handles before the permit wakes a pending runtime mutation.
    _permit: RuntimeCacheReadPermit<T>,
}

impl<T> fmt::Debug for RuntimeCacheRead<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeCacheRead")
            .finish_non_exhaustive()
    }
}

impl<T> Deref for RuntimeCacheRead<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

struct RuntimeCacheReadPermit<T> {
    inner: Arc<CacheInner<T>>,
}

impl<T> Drop for RuntimeCacheReadPermit<T> {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.readers = state.readers.saturating_sub(1);
        let readers_drained = state.readers == 0;
        drop(state);
        if readers_drained {
            self.inner.async_changed.notify_waiters();
        }
    }
}

pub(super) struct RuntimeCacheMutation<T> {
    inner: Arc<CacheInner<T>>,
    active: bool,
}

impl<T> RuntimeCacheMutation<T> {
    pub(super) fn publish(mut self, result: Result<Option<T>, String>) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.phase = CachePhase::Ready(match result {
            Ok(snapshot) => Ok(snapshot.map(Arc::new)),
            Err(error) => Err(Arc::from(error)),
        });
        self.active = false;
        drop(state);
        self.inner.async_changed.notify_waiters();
    }
}

impl<T> Drop for RuntimeCacheMutation<T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.phase = CachePhase::Uninitialized;
        drop(state);
        self.inner.async_changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
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
}
