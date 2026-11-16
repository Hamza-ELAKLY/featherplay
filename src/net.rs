//! One HTTP client for the whole app. Its TLS setup and connection pool are made
//! once and shared by the audio streams and the thumbnails, instead of every request
//! loading its own copy of the trusted certificates.

use std::sync::OnceLock;
use std::time::Duration;

// A stalled request fails rather than freezing playback: the audio thread reads streams.
const TIMEOUT: Duration = Duration::from_secs(15);

pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).build().into())
}
