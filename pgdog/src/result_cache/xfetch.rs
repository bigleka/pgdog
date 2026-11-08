//! XFetch: probabilistic early refresh of cache entries.
//!
//! Instead of every client missing at once when a hot entry expires, a hit
//! close to expiry occasionally refreshes the entry ahead of time. The
//! closer to expiry and the slower the query, the more likely a refresh.
//!
//! Vattani, Chierichetti, Lowenstein: "Optimal Probabilistic Cache Stampede
//! Prevention", VLDB 2015.

use std::time::Duration;

use rand::{rng, Rng};

/// Format version of stored entries.
pub const ENTRY_VERSION: u8 = 1;

/// Version byte + compute time in milliseconds (u32, big endian).
const HEADER_LEN: usize = 5;

/// A cached result with the time it took to compute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// How long the query took. Slower queries are refreshed earlier.
    pub delta: Duration,
    /// Postgres protocol messages sent to the client.
    pub payload: Vec<u8>,
}

impl Entry {
    pub fn encode(&self) -> Vec<u8> {
        let delta_ms = u32::try_from(self.delta.as_millis()).unwrap_or(u32::MAX);
        let mut bytes = Vec::with_capacity(HEADER_LEN + self.payload.len());
        bytes.push(ENTRY_VERSION);
        bytes.extend_from_slice(&delta_ms.to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    /// `None` if `bytes` isn't an entry in the current format.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (header, payload) = bytes.split_at_checked(HEADER_LEN)?;
        let (&version, delta_ms) = header.split_first()?;
        if version != ENTRY_VERSION {
            return None;
        }
        let delta_ms = u32::from_be_bytes(delta_ms.try_into().ok()?);

        Some(Self {
            delta: Duration::from_millis(delta_ms.into()),
            payload: payload.to_vec(),
        })
    }
}

/// Whether a hit should refresh the entry now, given `u` drawn uniformly from (0, 1].
///
/// Refresh when `-delta * beta * ln(u) >= remaining`. `beta` > 1 refreshes
/// earlier, < 1 later, and 0 disables early refresh.
pub fn should_refresh(remaining: Duration, delta: Duration, beta: f64, u: f64) -> bool {
    if beta <= 0.0 {
        return false;
    }
    let threshold = -delta.as_secs_f64() * beta * u.ln();
    threshold >= remaining.as_secs_f64() && threshold > 0.0
}

/// Uniform draw from (0, 1], so `ln(u)` is finite.
pub fn draw() -> f64 {
    1.0 - rng().random::<f64>()
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn test_refresh_when_threshold_reaches_remaining_ttl() {
        // -ln(e^-2) = 2, so threshold = delta * beta * 2 = 2s.
        let u = (-2.0f64).exp();
        assert!(should_refresh(Duration::from_millis(1900), SECOND, 1.0, u));
        assert!(!should_refresh(Duration::from_millis(2100), SECOND, 1.0, u));
    }

    #[test]
    fn test_beta_scales_threshold() {
        let u = (-2.0f64).exp();
        assert!(should_refresh(Duration::from_millis(3900), SECOND, 2.0, u));
        assert!(!should_refresh(Duration::from_millis(4100), SECOND, 2.0, u));
    }

    #[test]
    fn test_zero_beta_never_refreshes() {
        assert!(!should_refresh(Duration::ZERO, SECOND, 0.0, 0.01));
    }

    #[test]
    fn test_u_of_one_never_refreshes_before_expiry() {
        assert!(!should_refresh(Duration::from_millis(1), SECOND, 1.0, 1.0));
    }

    #[test]
    fn test_random_draw_is_in_unit_interval() {
        for _ in 0..1000 {
            let u = draw();
            assert!(u > 0.0 && u <= 1.0, "u = {u}");
        }
    }

    #[test]
    fn test_entry_round_trip() {
        let entry = Entry {
            delta: Duration::from_millis(1234),
            payload: b"rows".to_vec(),
        };
        let decoded = Entry::decode(&entry.encode()).unwrap();
        assert_eq!(decoded, entry);
    }

    #[test]
    fn test_entry_decode_rejects_malformed() {
        assert_eq!(Entry::decode(b""), None);
        assert_eq!(Entry::decode(&[ENTRY_VERSION, 0, 0]), None);
        assert_eq!(Entry::decode(&[ENTRY_VERSION + 1, 0, 0, 0, 0, b'x']), None);
    }
}
