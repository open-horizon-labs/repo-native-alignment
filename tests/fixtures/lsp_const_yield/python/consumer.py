from constants import Config, DEFAULT_PORT, FEATURE_FLAG, RETRY_LIMIT, TIMEOUT_MS


def worker_retry_limit() -> int:
    return RETRY_LIMIT


def worker_timeout() -> int:
    return TIMEOUT_MS


def worker_port() -> int:
    return DEFAULT_PORT


def worker_feature() -> bool:
    return FEATURE_FLAG


def worker_config() -> Config:
    return Config(RETRY_LIMIT)
