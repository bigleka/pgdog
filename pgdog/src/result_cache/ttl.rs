//! Adaptive TTL: hot entries live longer, up to a hard cap.
//!
//! Each hit pushes the entry's expiry to `ttl` from now, so entries keep
//! living while they're being read. `max_age`, counted from when the query
//! ran, caps that: it bounds how long a change made outside PgDog can stay
//! invisible.

use std::time::Duration;

/// New TTL for an entry that was just hit, or `None` to leave it as is.
///
/// Never shortens the current `remaining` TTL and never lets the entry
/// outlive `max_age`.
pub fn extended_ttl(
    age: Duration,
    remaining: Duration,
    ttl: Duration,
    max_age: Duration,
) -> Option<Duration> {
    let left = max_age.checked_sub(age)?;
    let extended = ttl.min(left);
    (extended > remaining).then_some(extended)
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use super::*;

    const fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    #[test]
    fn test_hit_slides_expiry_forward() {
        // Computed 10s ago, expires in 5s; a hit pushes it to 30s from now.
        assert_eq!(
            extended_ttl(secs(10), secs(5), secs(30), secs(300)),
            Some(secs(30))
        );
    }

    #[test]
    fn test_extension_stops_at_max_age() {
        // 290s old with a 300s cap: at most 10s left.
        assert_eq!(
            extended_ttl(secs(290), secs(5), secs(30), secs(300)),
            Some(secs(10))
        );
    }

    #[test]
    fn test_never_shortens() {
        assert_eq!(extended_ttl(secs(10), secs(40), secs(30), secs(300)), None);
        assert_eq!(extended_ttl(secs(290), secs(10), secs(30), secs(300)), None);
    }

    #[test]
    fn test_past_max_age_is_not_extended() {
        assert_eq!(extended_ttl(secs(300), secs(1), secs(30), secs(300)), None);
        assert_eq!(extended_ttl(secs(400), secs(1), secs(30), secs(300)), None);
    }
}
