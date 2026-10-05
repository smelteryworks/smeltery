//! Server limits read from the environment are clamped to usable values. Its own test binary:
//! it changes process-wide `.env` values that every `Settings::from_env` reads.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use smeltery_core::config::{Settings, set_env_value};

#[test]
fn the_request_timeout_and_the_stream_limit_are_at_least_one() {
    // `REQUEST_TIMEOUT=0` answered every request 408 at once (W1-05).
    set_env_value("REQUEST_TIMEOUT", "0");
    set_env_value("SERVER_MAX_STREAMS", "0");
    let settings = Settings::from_env();
    assert_eq!(settings.request_timeout, Duration::from_secs(1));
    assert_eq!(settings.server_max_streams, 1);
    set_env_value("REQUEST_TIMEOUT", "45");
    set_env_value("SERVER_MAX_STREAMS", "64");
    let settings = Settings::from_env();
    assert_eq!(settings.request_timeout, Duration::from_secs(45));
    assert_eq!(settings.server_max_streams, 64);
}
