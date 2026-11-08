//! Cached results as stored in Redis.

use std::time::{Duration, SystemTime};

/// Format version of stored entries.
pub const ENTRY_VERSION: u8 = 2;

/// Version (u8) + compute time in µs (u32) + computed at, ms since epoch (u64). Big endian.
const HEADER_LEN: usize = 1 + 4 + 8;

/// A cached result and how it was computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// How long the query took. Slower queries are refreshed earlier.
    pub delta: Duration,
    /// When the query ran. Bounds how long adaptive TTL keeps the entry.
    pub computed_at: SystemTime,
    /// Postgres protocol messages sent to the client.
    pub payload: Vec<u8>,
}

impl Entry {
    pub fn encode(&self) -> Vec<u8> {
        // Microseconds: fast queries would round down to zero in milliseconds.
        let delta_us = u32::try_from(self.delta.as_micros()).unwrap_or(u32::MAX);
        let computed_at_ms = self
            .computed_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|since_epoch| u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);

        let mut bytes = Vec::with_capacity(HEADER_LEN + self.payload.len());
        bytes.push(ENTRY_VERSION);
        bytes.extend_from_slice(&delta_us.to_be_bytes());
        bytes.extend_from_slice(&computed_at_ms.to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    /// `None` if `bytes` isn't an entry in the current format.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (header, payload) = bytes.split_at_checked(HEADER_LEN)?;
        let (&version, header) = header.split_first()?;
        if version != ENTRY_VERSION {
            return None;
        }
        let (delta_us, computed_at_ms) = header.split_at(4);
        let delta_us = u32::from_be_bytes(delta_us.try_into().ok()?);
        let computed_at_ms = u64::from_be_bytes(computed_at_ms.try_into().ok()?);

        Some(Self {
            delta: Duration::from_micros(delta_us.into()),
            computed_at: SystemTime::UNIX_EPOCH + Duration::from_millis(computed_at_ms),
            payload: payload.to_vec(),
        })
    }

    /// How long ago the query ran, as of `now`.
    pub fn age(&self, now: SystemTime) -> Duration {
        now.duration_since(self.computed_at).unwrap_or_default()
    }
}

#[cfg(test)]
mod test {
    use std::time::{Duration, SystemTime};

    use super::*;

    fn entry() -> Entry {
        Entry {
            delta: Duration::from_millis(1234),
            computed_at: SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123),
            payload: b"rows".to_vec(),
        }
    }

    #[test]
    fn test_entry_round_trip() {
        let entry = entry();
        assert_eq!(Entry::decode(&entry.encode()), Some(entry));
    }

    #[test]
    fn test_entry_decode_rejects_malformed() {
        assert_eq!(Entry::decode(b""), None);
        assert_eq!(Entry::decode(&[ENTRY_VERSION, 0, 0]), None);

        let mut other_version = entry().encode();
        other_version[0] = ENTRY_VERSION + 1;
        assert_eq!(Entry::decode(&other_version), None);
    }

    /// Fast queries keep their compute time: XFetch needs it above zero.
    #[test]
    fn test_sub_millisecond_delta_round_trip() {
        let entry = Entry {
            delta: Duration::from_micros(250),
            ..entry()
        };
        assert_eq!(Entry::decode(&entry.encode()).unwrap().delta, entry.delta);
    }

    /// Entries stored before `computed_at` existed are read as misses.
    #[test]
    fn test_entry_decode_rejects_previous_format() {
        assert_eq!(Entry::decode(&[1, 0, 0, 0, 5, b'x']), None);
    }

    #[test]
    fn test_age() {
        let entry = entry();
        let later = entry.computed_at + Duration::from_secs(3);
        assert_eq!(entry.age(later), Duration::from_secs(3));
        // Clock went backwards: treat as brand new.
        assert_eq!(entry.age(SystemTime::UNIX_EPOCH), Duration::ZERO);
    }
}
