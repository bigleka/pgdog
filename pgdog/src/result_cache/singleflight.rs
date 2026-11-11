//! Coalesce concurrent cache misses for the same key.
//!
//! The first client to miss on a key becomes the [`Leader`] and runs the
//! query. Clients missing on the same key while it runs become [`Follower`]s
//! and wait for the leader's result instead of querying the database.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use tokio::sync::watch;

/// Result a leader shares with its followers.
type Payload = Arc<Vec<u8>>;

/// Keys with a query in flight.
#[derive(Debug, Clone, Default)]
pub struct Singleflight {
    inflight: Arc<Mutex<HashMap<String, watch::Receiver<Option<Payload>>>>>,
}

/// Role of a client that missed the cache.
#[derive(Debug)]
pub enum Flight {
    Leader(Leader),
    Follower(Follower),
}

impl Singleflight {
    /// Join the flight for `key`, leading it if nobody else is.
    pub fn join(&self, key: &str) -> Flight {
        let mut inflight = self.inflight.lock();
        if let Some(receiver) = inflight.get(key) {
            return Flight::Follower(Follower {
                receiver: receiver.clone(),
            });
        }

        let (sender, receiver) = watch::channel(None);
        inflight.insert(key.to_owned(), receiver);

        Flight::Leader(Leader {
            key: key.to_owned(),
            sender,
            inflight: self.inflight.clone(),
        })
    }
}

/// Client running the query for everyone waiting on its key.
///
/// Dropping it without calling [`Leader::finish`] releases the followers,
/// which then run the query themselves.
#[derive(Debug)]
pub struct Leader {
    key: String,
    sender: watch::Sender<Option<Payload>>,
    inflight: Arc<Mutex<HashMap<String, watch::Receiver<Option<Payload>>>>>,
}

impl Leader {
    /// Hand the query result to the followers.
    ///
    /// Call after the result is stored in the cache, so clients arriving
    /// after this flight ends find it there.
    pub fn finish(self, payload: Vec<u8>) {
        self.sender.send_replace(Some(Arc::new(payload)));
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        // Followers are released when `sender` is dropped right after this.
        self.inflight.lock().remove(&self.key);
    }
}

/// Client waiting for a leader's result.
#[derive(Debug)]
pub struct Follower {
    receiver: watch::Receiver<Option<Payload>>,
}

impl Follower {
    /// Leader's result, or `None` if it didn't produce one.
    pub async fn wait(mut self) -> Option<Payload> {
        self.receiver
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|payload| payload.clone())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn leader(flight: Flight) -> Leader {
        match flight {
            Flight::Leader(leader) => leader,
            Flight::Follower(_) => panic!("expected leader"),
        }
    }

    fn follower(flight: Flight) -> Follower {
        match flight {
            Flight::Follower(follower) => follower,
            Flight::Leader(_) => panic!("expected follower"),
        }
    }

    #[tokio::test]
    async fn test_followers_get_leader_result() {
        let singleflight = Singleflight::default();
        let leader = leader(singleflight.join("key"));
        let followers = [
            follower(singleflight.join("key")),
            follower(singleflight.join("key")),
        ];

        leader.finish(b"rows".to_vec());

        for follower in followers {
            assert_eq!(follower.wait().await.as_deref(), Some(&b"rows".to_vec()));
        }
    }

    #[tokio::test]
    async fn test_dropped_leader_releases_followers() {
        let singleflight = Singleflight::default();
        let leader = leader(singleflight.join("key"));
        let follower = follower(singleflight.join("key"));

        drop(leader);

        assert_eq!(follower.wait().await, None);
    }

    #[test]
    fn test_flight_ends_with_leader() {
        let singleflight = Singleflight::default();
        leader(singleflight.join("key")).finish(b"rows".to_vec());

        // Next miss on the key leads a new flight.
        leader(singleflight.join("key"));
    }

    #[test]
    fn test_keys_are_independent() {
        let singleflight = Singleflight::default();
        let _first = leader(singleflight.join("a"));
        leader(singleflight.join("b"));
    }
}
