use crate::{Config, DEFAULT_PORT, FEATURE_FLAG, RETRY_LIMIT, TIMEOUT_MS};

pub fn worker_retry_limit() -> usize {
    RETRY_LIMIT
}

pub fn worker_timeout() -> u64 {
    TIMEOUT_MS
}

pub fn worker_port() -> u16 {
    DEFAULT_PORT
}

pub fn worker_feature() -> bool {
    FEATURE_FLAG
}

pub fn worker_config() -> Config {
    Config {
        retries: RETRY_LIMIT,
    }
}
