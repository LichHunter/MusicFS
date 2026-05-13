use musicfs_cas::CasError;
use musicfs_core::Error;
use std::time::{Duration, Instant};

pub fn assert_error_contains<T, E: std::fmt::Debug>(result: Result<T, E>, expected_text: &str) {
    match result {
        Ok(_) => panic!("Expected error containing '{}', but got Ok", expected_text),
        Err(e) => {
            let error_msg = format!("{:?}", e);
            assert!(
                error_msg.contains(expected_text),
                "Expected error containing '{}', but got: {}",
                expected_text,
                error_msg
            );
        }
    }
}

pub fn assert_io_error<T>(result: Result<T, Error>) {
    match result {
        Err(Error::Io(_)) => (),
        Err(e) => panic!("Expected Io error, got: {:?}", e),
        Ok(_) => panic!("Expected Io error, got Ok"),
    }
}

pub fn assert_cas_io_error<T>(result: Result<T, CasError>) {
    match result {
        Err(CasError::Io(_)) => (),
        Err(e) => panic!("Expected CasError::Io, got: {:?}", e),
        Ok(_) => panic!("Expected CasError::Io, got Ok"),
    }
}

pub fn assert_cas_not_found<T>(result: Result<T, CasError>) {
    match result {
        Err(CasError::NotFound(_)) => (),
        Err(e) => panic!("Expected CasError::NotFound, got: {:?}", e),
        Ok(_) => panic!("Expected CasError::NotFound, got Ok"),
    }
}

pub fn assert_cas_integrity_error<T>(result: Result<T, CasError>) {
    match result {
        Err(CasError::IntegrityError { .. }) => (),
        Err(e) => panic!("Expected CasError::IntegrityError, got: {:?}", e),
        Ok(_) => panic!("Expected CasError::IntegrityError, got Ok"),
    }
}

pub fn assert_file_not_found<T>(result: Result<T, Error>) {
    match result {
        Err(Error::FileNotFound(_)) => (),
        Err(e) => panic!("Expected FileNotFound error, got: {:?}", e),
        Ok(_) => panic!("Expected FileNotFound error, got Ok"),
    }
}

pub fn assert_origin_error<T>(result: Result<T, Error>) {
    match result {
        Err(Error::Origin(_)) => (),
        Err(e) => panic!("Expected Origin error, got: {:?}", e),
        Ok(_) => panic!("Expected Origin error, got Ok"),
    }
}

pub fn assert_timeout_error<T>(result: Result<T, Error>) {
    match result {
        Err(Error::Timeout(_)) => (),
        Err(e) => panic!("Expected Timeout error, got: {:?}", e),
        Ok(_) => panic!("Expected Timeout error, got Ok"),
    }
}

pub struct TimedAssertion {
    start: Instant,
    min_duration: Option<Duration>,
    max_duration: Option<Duration>,
}

impl TimedAssertion {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            min_duration: None,
            max_duration: None,
        }
    }

    pub fn expect_at_least(mut self, duration: Duration) -> Self {
        self.min_duration = Some(duration);
        self
    }

    pub fn expect_at_most(mut self, duration: Duration) -> Self {
        self.max_duration = Some(duration);
        self
    }

    pub fn assert_elapsed(self) {
        let elapsed = self.start.elapsed();

        if let Some(min) = self.min_duration {
            assert!(
                elapsed >= min,
                "Expected at least {:?}, but only {:?} elapsed",
                min,
                elapsed
            );
        }

        if let Some(max) = self.max_duration {
            assert!(
                elapsed <= max,
                "Expected at most {:?}, but {:?} elapsed",
                max,
                elapsed
            );
        }
    }
}

impl Default for TimedAssertion {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn assert_completes_within<F, T>(future: F, timeout: Duration) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(timeout, future)
        .await
        .expect(&format!(
            "Operation did not complete within {:?}",
            timeout
        ))
}

pub async fn assert_times_out<F, T>(future: F, timeout: Duration)
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(_) => panic!("Expected operation to time out, but it completed"),
        Err(_) => (),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assert_error_contains() {
        let result: Result<(), Error> = Err(Error::Origin("connection refused".into()));
        assert_error_contains(result, "connection");
    }

    #[test]
    #[should_panic(expected = "Expected error containing")]
    fn test_assert_error_contains_failure() {
        let result: Result<(), Error> = Err(Error::Origin("something else".into()));
        assert_error_contains(result, "connection");
    }

    #[test]
    fn test_assert_io_error() {
        let result: Result<(), Error> =
            Err(Error::Io(std::io::Error::new(std::io::ErrorKind::Other, "test")));
        assert_io_error(result);
    }

    #[test]
    fn test_timed_assertion_at_least() {
        let timer = TimedAssertion::new().expect_at_least(Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(15));
        timer.assert_elapsed();
    }

    #[test]
    fn test_timed_assertion_at_most() {
        let timer = TimedAssertion::new().expect_at_most(Duration::from_millis(100));
        timer.assert_elapsed();
    }

    #[tokio::test]
    async fn test_assert_completes_within() {
        let result =
            assert_completes_within(async { 42 }, Duration::from_millis(100)).await;
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_assert_times_out() {
        assert_times_out(
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
            },
            Duration::from_millis(10),
        )
        .await;
    }
}
