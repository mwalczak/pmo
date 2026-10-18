//! Requests per key per minute, so one leaked or buggy key cannot flood the API. A token bucket per key, in
//! memory (the API runs as one replica): bursts up to the limit, refilled evenly over the minute.
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use uuid::Uuid;

pub struct RateLimiter {
    per_minute: u32,
    buckets: Mutex<HashMap<Uuid, Bucket>>,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl RateLimiter {
    /// `per_minute` 0 turns limiting off.
    pub fn new(per_minute: u32) -> Self {
        Self { per_minute, buckets: Mutex::default() }
    }

    /// Takes one request from the key's bucket, or says how many seconds until the next one is allowed.
    pub fn check(&self, key_id: Uuid) -> Result<(), u64> {
        self.check_at(key_id, Instant::now())
    }

    fn check_at(&self, key_id: Uuid, now: Instant) -> Result<(), u64> {
        if self.per_minute == 0 {
            return Ok(());
        }
        let capacity = f64::from(self.per_minute);
        let per_second = capacity / 60.0;
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets.entry(key_id).or_insert(Bucket { tokens: capacity, at: now });
        bucket.tokens = (bucket.tokens + now.duration_since(bucket.at).as_secs_f64() * per_second).min(capacity);
        bucket.at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / per_second).as_secs().max(1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_a_burst_then_refills_over_the_minute() {
        let limiter = RateLimiter::new(60);
        let (key, other, start) = (Uuid::now_v7(), Uuid::now_v7(), Instant::now());
        for _ in 0..60 {
            assert!(limiter.check_at(key, start).is_ok());
        }
        assert_eq!(limiter.check_at(key, start), Err(1));
        assert!(limiter.check_at(other, start).is_ok(), "keys have their own buckets");
        assert!(limiter.check_at(key, start + Duration::from_secs(1)).is_ok(), "one more per second");
        assert!(limiter.check_at(key, start + Duration::from_secs(1)).is_err());
    }

    #[test]
    fn zero_is_off() {
        let limiter = RateLimiter::new(0);
        let key = Uuid::now_v7();
        assert!((0..10_000).all(|_| limiter.check(key).is_ok()));
    }
}
