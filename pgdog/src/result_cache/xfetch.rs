use rand::Rng;

/// Evaluates the XFetch probabilistic early recomputation condition.
///
/// Returns true if a background recomputation should be triggered.
///
/// Formula: `-beta * delta * ln(u) > rem_ttl`
/// where:
/// - `rem_ttl_secs`: remaining time to live in seconds
/// - `delta_secs`: estimated compute/query execution time in seconds (e.g. 0.1s)
/// - `beta`: aggressiveness multiplier (standard default: 1.0)
pub fn should_refresh(rem_ttl_secs: f64, delta_secs: f64, beta: f64) -> bool {
    if rem_ttl_secs <= 0.0 {
        return true;
    }
    if delta_secs <= 0.0 || beta <= 0.0 {
        return false;
    }

    let mut rng = rand::rng();
    let u: f64 = rng.random_range(0.0001..1.0); // Avoid ln(0.0)

    let threshold = -beta * delta_secs * u.ln();
    threshold > rem_ttl_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xfetch_never_triggers_when_ttl_is_huge() {
        let mut triggered = false;
        for _ in 0..100 {
            if should_refresh(1000.0, 0.1, 1.0) {
                triggered = true;
                break;
            }
        }
        assert!(!triggered);
    }

    #[test]
    fn test_xfetch_triggers_when_ttl_is_zero_or_negative() {
        assert!(should_refresh(0.0, 0.1, 1.0));
        assert!(should_refresh(-1.0, 0.1, 1.0));
    }

    #[test]
    fn test_xfetch_probabilistic_near_expiration() {
        let mut count = 0;
        for _ in 0..100 {
            if should_refresh(0.01, 0.5, 1.0) {
                count += 1;
            }
        }
        assert!(count > 50);
    }
}
