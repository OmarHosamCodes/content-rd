//! Server side of the local browser bridge: Instagram tool calls wait here until the user's machine answers.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, oneshot};

use crate::ToolResult;

const OFFLINE: &str = "The Instagram bridge is offline. On your computer run `content-rd bridge`, keep its Chrome window open and logged in to Instagram, then retry.";

#[derive(Serialize, Deserialize, Debug)]
pub struct Job {
    pub id: u64,
    pub tool: String,
    pub args: Value,
}

#[derive(Default)]
struct Inner {
    queue: VecDeque<Job>,
    pending: HashMap<u64, oneshot::Sender<ToolResult<Value>>>,
    next_id: u64,
    last_poll: Option<Instant>,
}

pub struct Relay {
    inner: Mutex<Inner>,
    notify: Notify,
    job_timeout: Duration,
    poll_wait: Duration,
}

impl Relay {
    pub fn new(job_timeout: Duration, poll_wait: Duration) -> Relay {
        Relay { inner: Mutex::default(), notify: Notify::new(), job_timeout, poll_wait }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()) // state stays usable even if a holder panicked
    }

    /// Run a tool on the bridge. Fails at once when no bridge has polled recently.
    pub async fn call(&self, tool: &str, args: Value) -> ToolResult<Value> {
        let (id, rx) = {
            let mut s = self.inner();
            // the bridge polls continuously, so a gap longer than one poll plus slack means it is gone
            if s.last_poll.is_none_or(|t| t.elapsed() > self.poll_wait + Duration::from_secs(15)) {
                return Err(OFFLINE.into());
            }
            s.next_id += 1;
            let (tx, rx) = oneshot::channel();
            let id = s.next_id;
            s.pending.insert(id, tx);
            s.queue.push_back(Job { id, tool: tool.to_string(), args });
            (id, rx)
        };
        self.notify.notify_one();
        match tokio::time::timeout(self.job_timeout, rx).await {
            Ok(Ok(result)) => result,
            _ => {
                let mut s = self.inner();
                s.pending.remove(&id);
                s.queue.retain(|j| j.id != id);
                Err(format!("The Instagram bridge timed out after {}s. Check the Chrome window on your computer.", self.job_timeout.as_secs()))
            }
        }
    }

    /// Long-poll used by the bridge: the next job, or None after `poll_wait`.
    pub async fn next_job(&self) -> Option<Job> {
        let deadline = tokio::time::Instant::now() + self.poll_wait;
        loop {
            {
                let mut s = self.inner();
                s.last_poll = Some(Instant::now());
                if let Some(job) = s.queue.pop_front() {
                    return Some(job);
                }
            }
            if tokio::time::timeout_at(deadline, self.notify.notified()).await.is_err() {
                self.inner().last_poll = Some(Instant::now());
                return None;
            }
        }
    }

    /// Deliver a result. Unknown or already timed-out ids are ignored.
    pub fn finish(&self, id: u64, result: ToolResult<Value>) {
        if let Some(tx) = self.inner().pending.remove(&id) {
            let _ = tx.send(result);
        }
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.inner().pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn relay() -> Arc<Relay> {
        Arc::new(Relay::new(Duration::from_millis(300), Duration::from_millis(100)))
    }

    #[tokio::test]
    async fn offline_bridge_fails_fast() {
        let r = relay();
        let started = Instant::now();
        assert!(r.call("find_reels", json!({})).await.unwrap_err().contains("bridge is offline"));
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn roundtrip_and_bridge_errors() {
        let r = relay();
        let bridge = r.clone();
        tokio::spawn(async move {
            loop {
                if let Some(job) = bridge.next_job().await {
                    let out = if job.tool == "ok" { Ok(json!({"echo": job.args})) } else { Err("boom".to_string()) };
                    bridge.finish(job.id, out);
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(r.call("ok", json!({"a": 1})).await.unwrap(), json!({"echo": {"a": 1}}));
        assert_eq!(r.call("bad", json!({})).await.unwrap_err(), "boom");
    }

    #[tokio::test]
    async fn stuck_job_times_out_and_is_forgotten() {
        let r = relay();
        let bridge = r.clone();
        let taken = tokio::spawn(async move { bridge.next_job().await.unwrap() });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(r.call("slow", json!({})).await.unwrap_err().contains("timed out"));
        let job = taken.await.unwrap();
        r.finish(job.id, Ok(json!(1))); // late answer: ignored, no panic
        r.finish(999, Ok(json!(1))); // unknown id: ignored
        assert_eq!(r.pending_len(), 0);
    }

    #[tokio::test]
    async fn empty_poll_returns_none() {
        assert!(relay().next_job().await.is_none());
    }
}
