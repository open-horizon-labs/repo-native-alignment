RETRY_LIMIT = 3
TIMEOUT_MS = 250
DEFAULT_PORT = 8080
FEATURE_FLAG = True
UNUSED_SENTINEL = 99


class Config:
    def __init__(self, retries: int) -> None:
        self.retries = retries


def local_retry_limit() -> int:
    return RETRY_LIMIT


def local_timeout() -> int:
    return TIMEOUT_MS


def local_port() -> int:
    return DEFAULT_PORT


def local_feature() -> bool:
    return FEATURE_FLAG


def make_config() -> Config:
    return Config(RETRY_LIMIT)
