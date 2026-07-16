pub mod worker;

pub const RETRY_LIMIT: usize = 3;
pub const TIMEOUT_MS: u64 = 250;
pub const DEFAULT_PORT: u16 = 8080;
pub const FEATURE_FLAG: bool = true;
pub const UNUSED_SENTINEL: usize = 99;

pub struct Config {
    pub retries: usize,
}

pub trait Retryable {
    fn retries(&self) -> usize;
}

impl Retryable for Config {
    fn retries(&self) -> usize {
        self.retries
    }
}

pub fn local_retry_limit() -> usize {
    RETRY_LIMIT
}

pub fn local_timeout() -> u64 {
    TIMEOUT_MS
}

pub fn local_port() -> u16 {
    DEFAULT_PORT
}

pub fn local_feature() -> bool {
    FEATURE_FLAG
}

pub fn make_config() -> Config {
    Config {
        retries: RETRY_LIMIT,
    }
}
