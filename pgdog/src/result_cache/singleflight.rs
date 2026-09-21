use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

#[derive(Clone, Debug)]
pub enum SingleflightResult {
    Success(Vec<u8>),
    Error,
}

#[derive(Debug)]
pub enum SingleflightStatus {
    Leader(SingleflightGuard),
    Follower(broadcast::Receiver<SingleflightResult>),
}

#[derive(Debug)]
pub struct SingleflightGuard {
    group: Arc<Mutex<HashMap<String, broadcast::Sender<SingleflightResult>>>>,
    key: String,
    completed: bool,
}

impl SingleflightGuard {
    pub async fn finish(mut self, result: SingleflightResult) {
        self.completed = true;
        let mut map = self.group.lock().await;
        if let Some(sender) = map.remove(&self.key) {
            let _ = sender.send(result);
        }
    }
}

impl Drop for SingleflightGuard {
    fn drop(&mut self) {
        if !self.completed {
            let group = self.group.clone();
            let key = self.key.clone();
            tokio::spawn(async move {
                let mut map = group.lock().await;
                if let Some(sender) = map.remove(&key) {
                    let _ = sender.send(SingleflightResult::Error);
                }
            });
        }
    }
}

#[derive(Debug, Clone)]
pub struct SingleflightGroup {
    map: Arc<Mutex<HashMap<String, broadcast::Sender<SingleflightResult>>>>,
}

impl Default for SingleflightGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl SingleflightGroup {
    pub fn new() -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Try to acquire leader status or join as follower for a cache key.
    pub async fn enter(&self, key: &str) -> SingleflightStatus {
        let mut map = self.map.lock().await;
        if let Some(sender) = map.get(key) {
            let rx = sender.subscribe();
            SingleflightStatus::Follower(rx)
        } else {
            let (tx, _) = broadcast::channel(16);
            map.insert(key.to_string(), tx);
            SingleflightStatus::Leader(SingleflightGuard {
                group: self.map.clone(),
                key: key.to_string(),
                completed: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_singleflight_leader_and_followers() {
        let group = SingleflightGroup::new();
        let key = "test_key";

        // First task enters -> Leader
        let status1 = group.enter(key).await;
        let guard = match status1 {
            SingleflightStatus::Leader(guard) => guard,
            _ => panic!("Expected Leader"),
        };

        // Second task enters -> Follower
        let status2 = group.enter(key).await;
        let mut rx2 = match status2 {
            SingleflightStatus::Follower(rx) => rx,
            _ => panic!("Expected Follower"),
        };

        // Leader finishes with success
        let payload = vec![1, 2, 3, 4];
        guard.finish(SingleflightResult::Success(payload.clone())).await;

        // Follower receives payload
        let res = tokio::time::timeout(Duration::from_millis(500), rx2.recv()).await;
        assert!(res.is_ok());
        match res.unwrap().unwrap() {
            SingleflightResult::Success(bytes) => assert_eq!(bytes, payload),
            _ => panic!("Expected Success"),
        }
    }

    #[tokio::test]
    async fn test_singleflight_leader_drop_notifies_error() {
        let group = SingleflightGroup::new();
        let key = "test_drop_key";

        let status1 = group.enter(key).await;
        let guard = match status1 {
            SingleflightStatus::Leader(guard) => guard,
            _ => panic!("Expected Leader"),
        };

        let status2 = group.enter(key).await;
        let mut rx2 = match status2 {
            SingleflightStatus::Follower(rx) => rx,
            _ => panic!("Expected Follower"),
        };

        // Drop guard without finish
        drop(guard);

        // Follower should receive Error or closed
        let res = tokio::time::timeout(Duration::from_millis(500), rx2.recv()).await;
        assert!(res.is_ok());
        match res.unwrap() {
            Ok(SingleflightResult::Error) => (),
            Err(broadcast::error::RecvError::Closed) => (),
            other => panic!("Expected Error or Closed, got {:?}", other),
        }
    }
}
