use crate::{
    Config, DEFAULT_PORT, FEATURE_FLAG, MUTABLE_LIMIT, RETRY_LIMIT, STATIC_TIMEOUT_MS, TIMEOUT_MS,
};

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

pub fn worker_static_timeout() -> u64 {
    STATIC_TIMEOUT_MS
}

pub fn worker_mutable_limit() -> usize {
    unsafe { MUTABLE_LIMIT }
}

pub fn worker_associated_limit() -> usize {
    Config::ASSOCIATED_LIMIT
}

pub fn worker_config() -> Config {
    Config {
        retries: RETRY_LIMIT,
    }
}
