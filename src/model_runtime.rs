//! 同名模型共享可动态调整的并发池；排队与配置更新不持有业务事务。
use crate::error::{Error, Result};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub(crate) const DEFAULT_CONCURRENCY: u32 = 4;

#[derive(Clone, Copy)]
pub(crate) struct ModelLimit {
    pub max: u32,
    pub revision: i64,
}
impl ModelLimit {
    pub fn new(max: u32, revision: i64) -> Result<Self> {
        if !(1..=64).contains(&max) || revision < 0 {
            return Err(Error::invalid("模型最大并发须为 1～64，配置修订须有效"));
        }
        Ok(Self { max, revision })
    }
}
impl Default for ModelLimit {
    fn default() -> Self {
        Self {
            max: DEFAULT_CONCURRENCY,
            revision: 0,
        }
    }
}

struct State {
    limit: ModelLimit,
    active: u32,
}
struct Gate {
    state: Mutex<State>,
    admission: AsyncMutex<()>,
    changed: Notify,
}
impl Gate {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn apply(&self, limit: ModelLimit) {
        let mut state = self.state();
        if limit.revision > state.limit.revision {
            state.limit = limit;
            drop(state);
            self.changed.notify_waiters();
        }
    }
    async fn acquire(self: Arc<Self>) -> ModelPermit {
        // FIFO admission 使只有队首等待容量；取消队首会自动把顺序交给下一位。
        let _turn = self.admission.lock().await;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.state();
                if state.active < state.limit.max {
                    state.active += 1;
                    return ModelPermit { gate: self.clone() };
                }
            }
            changed.await;
        }
    }
}

pub(crate) struct ModelPermit {
    gate: Arc<Gate>,
}
impl Drop for ModelPermit {
    fn drop(&mut self) {
        let mut state = self.gate.state();
        state.active -= 1;
        drop(state);
        self.gate.changed.notify_waiters();
    }
}

#[derive(Default)]
pub(crate) struct ModelRuntime {
    models: Mutex<HashMap<String, Arc<Gate>>>,
}
impl ModelRuntime {
    fn gate(&self, name: &str, limit: ModelLimit) -> Arc<Gate> {
        let gate = self
            .models
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(name.to_owned())
            .or_insert_with(|| {
                Arc::new(Gate {
                    state: Mutex::new(State { limit, active: 0 }),
                    admission: AsyncMutex::new(()),
                    changed: Notify::new(),
                })
            })
            .clone();
        gate.apply(limit);
        gate
    }
    pub fn apply(&self, name: &str, limit: ModelLimit) {
        self.gate(name, limit);
    }
    pub async fn acquire(&self, name: &str, limit: ModelLimit) -> ModelPermit {
        self.gate(name, limit).acquire().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::{Future, poll_fn},
        pin::Pin,
        task::Poll,
    };
    async fn pending<F: Future>(mut future: Pin<&mut F>) {
        poll_fn(|cx| match future.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("模型额度已满却放行"),
        })
        .await;
    }
    #[tokio::test]
    async fn same_model_is_fifo_and_cancelled_waiters_do_not_leak_slots() {
        let pool = ModelRuntime::default();
        let limit = ModelLimit::new(1, 1).unwrap();
        let first = pool.acquire("a", limit).await;
        let mut cancelled = Box::pin(pool.acquire("a", limit));
        pending(cancelled.as_mut()).await;
        let mut a = Box::pin(pool.acquire("a", limit));
        pending(a.as_mut()).await;
        let mut b = Box::pin(pool.acquire("a", limit));
        pending(b.as_mut()).await;
        drop(cancelled);
        pending(a.as_mut()).await;
        let unrelated = pool.acquire("b", limit).await;
        drop(first);
        let a = a.await;
        pending(b.as_mut()).await;
        drop(a);
        drop(b.await);
        drop(unrelated);
        drop(pool.acquire("a", limit).await);
    }
    #[tokio::test]
    async fn shrinking_keeps_inflight_debt_and_stale_limits_cannot_expand_capacity() {
        let pool = ModelRuntime::default();
        let original = ModelLimit::new(3, 1).unwrap();
        let a = pool.acquire("a", original).await;
        let b = pool.acquire("a", original).await;
        let c = pool.acquire("a", original).await;
        pool.apply("a", ModelLimit::new(1, 2).unwrap());
        let mut next = Box::pin(pool.acquire("a", original));
        pending(next.as_mut()).await;
        drop(a);
        pending(next.as_mut()).await;
        drop(b);
        pending(next.as_mut()).await;
        drop(c);
        let next = next.await;
        let mut waiting = Box::pin(pool.acquire("a", ModelLimit::new(64, 1).unwrap()));
        pending(waiting.as_mut()).await;
        pool.apply("a", ModelLimit::new(2, 3).unwrap());
        let waiting = waiting.await;
        drop(next);
        drop(waiting);
    }
    #[tokio::test]
    async fn aborting_an_active_caller_releases_its_permit() {
        let pool = Arc::new(ModelRuntime::default());
        let started = Arc::new(Notify::new());
        let limit = ModelLimit::new(1, 1).unwrap();
        let task = tokio::spawn({
            let pool = pool.clone();
            let started = started.clone();
            async move {
                let _permit = pool.acquire("a", limit).await;
                started.notify_one();
                std::future::pending::<()>().await;
            }
        });
        started.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let permit =
            tokio::time::timeout(std::time::Duration::from_secs(1), pool.acquire("a", limit))
                .await
                .unwrap();
        drop(permit);
    }
}
