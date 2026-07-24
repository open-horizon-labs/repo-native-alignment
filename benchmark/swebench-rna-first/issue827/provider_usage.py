#!/usr/bin/env python3
"""Strict, serializable Claude provider-usage evidence.

Provider token counts are selection evidence.  Missing counters must therefore
remain missing rather than becoming convenient zeroes, and CLI turns must not
be relabeled as provider requests.  Claude Code 2.1.216's whole-invocation
``modelUsage`` is authoritative; its differently scoped top-level ``usage`` is
retained separately.  Transcript events cover only agent-visible assistant
responses, while ``modelUsage`` also includes auxiliary Claude CLI requests
such as session-title generation.  Reasoning tokens remain explicitly null
and unobserved when the provider does not expose them.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from typing import Any


SCHEMA_VERSION = "issue827-provider-usage-v1"
REQUIRED_TOKEN_FIELDS = (
    "input_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "output_tokens",
)
TOKEN_FIELDS = (
    *REQUIRED_TOKEN_FIELDS,
    "reasoning_tokens",
)
ALIASES = {
    "input_tokens": ("input_tokens", "inputTokens"),
    "cache_creation_input_tokens": (
        "cache_creation_input_tokens",
        "cacheCreationInputTokens",
    ),
    "cache_read_input_tokens": (
        "cache_read_input_tokens",
        "cacheReadInputTokens",
    ),
    "output_tokens": ("output_tokens", "outputTokens"),
    "reasoning_tokens": ("reasoning_tokens", "reasoningTokens"),
}


class ProviderUsageError(ValueError):
    """Usage evidence was absent, malformed, contradictory, or zero-only."""

    def __init__(self, errors: Sequence[str], receipt: Mapping[str, Any]):
        self.errors = tuple(errors)
        self.receipt = dict(receipt)
        super().__init__(";".join(self.errors))


def _optional_counter(value: object, label: str, errors: list[str]) -> int | None:
    if value is None:
        return None
    if type(value) is not int or value < 0:
        errors.append(f"{label}_must_be_nonnegative_integer_or_null")
        return None
    return value


def _required_counter(
    usage: Mapping[str, Any],
    field: str,
    errors: list[str],
) -> int | None:
    aliases = ALIASES[field]
    present = [(name, usage[name]) for name in aliases if name in usage]
    if not present:
        errors.append(f"{field}_missing")
        return None
    values: list[int] = []
    for name, value in present:
        if type(value) is not int or value < 0:
            errors.append(f"{name}_must_be_nonnegative_integer")
        else:
            values.append(value)
    if len(values) != len(present):
        return None
    if len(set(values)) != 1:
        errors.append(f"{field}_aliases_inconsistent")
        return None
    return values[0]


def _reasoning_counter(
    usage: Mapping[str, Any],
    errors: list[str],
) -> int | None:
    candidates: list[int] = []
    direct_errors: list[str] = []
    direct_present = any(alias in usage for alias in ALIASES["reasoning_tokens"])
    if direct_present:
        direct = _required_counter(usage, "reasoning_tokens", direct_errors)
        if direct is not None:
            candidates.append(direct)

    detail_objects: list[Mapping[str, Any]] = []
    for key in ("output_tokens_details", "outputTokensDetails"):
        if key in usage:
            value = usage[key]
            if not isinstance(value, Mapping):
                errors.append(f"{key}_must_be_object")
            else:
                detail_objects.append(value)
    for detail in detail_objects:
        if any(alias in detail for alias in ALIASES["reasoning_tokens"]):
            detail_errors: list[str] = []
            value = _required_counter(detail, "reasoning_tokens", detail_errors)
            errors.extend(detail_errors)
            if value is not None:
                candidates.append(value)

    errors.extend(direct_errors)
    if not candidates:
        return None
    if len(set(candidates)) > 1:
        errors.append("reasoning_tokens_inconsistent")
        return None
    return candidates[0] if candidates else None


def _normalize_usage(
    usage: object,
    where: str,
    errors: list[str],
) -> dict[str, int | None] | None:
    if not isinstance(usage, Mapping):
        errors.append(f"{where}_must_be_object")
        return None
    normalized: dict[str, int | None] = {
        field: _required_counter(usage, field, errors)
        for field in REQUIRED_TOKEN_FIELDS
    }
    normalized["reasoning_tokens"] = _reasoning_counter(usage, errors)
    if any(normalized[field] is None for field in REQUIRED_TOKEN_FIELDS):
        return None
    return normalized


def _model_usage_total(
    value: object,
    errors: list[str],
) -> dict[str, int | None] | None:
    if not isinstance(value, Mapping) or not value:
        errors.append("modelUsage_must_be_nonempty_object")
        return None
    total: dict[str, int | None] = {
        **{field: 0 for field in REQUIRED_TOKEN_FIELDS},
        "reasoning_tokens": 0,
    }
    reasoning_observed = True
    valid = True
    for model_name in sorted(value, key=str):
        entry = value[model_name]
        if not isinstance(model_name, str) or not model_name:
            errors.append("modelUsage_model_name_invalid")
            valid = False
            continue
        if not isinstance(entry, Mapping):
            errors.append(f"modelUsage_{model_name}_must_be_object")
            valid = False
            continue
        candidate = entry.get("usage", entry)
        observed = _normalize_usage(
            candidate, f"modelUsage_{model_name}_usage", errors
        )
        if observed is None:
            valid = False
            continue
        for field in REQUIRED_TOKEN_FIELDS:
            total[field] = int(total[field]) + int(observed[field])
        if observed["reasoning_tokens"] is None:
            reasoning_observed = False
        elif reasoning_observed:
            total["reasoning_tokens"] = int(total["reasoning_tokens"]) + int(
                observed["reasoning_tokens"]
            )
    if not reasoning_observed:
        total["reasoning_tokens"] = None
    return total if valid else None


def _event_usage_total(
    model_events: object,
    errors: list[str],
) -> tuple[dict[str, int | None] | None, int | None]:
    if model_events is None:
        return None, None
    if not isinstance(model_events, Sequence) or isinstance(
        model_events, (str, bytes, bytearray)
    ):
        errors.append("model_events_must_be_sequence_or_null")
        return None, None
    total: dict[str, int | None] = {
        **{field: 0 for field in REQUIRED_TOKEN_FIELDS},
        "reasoning_tokens": 0,
    }
    reasoning_observed = True
    count = 0
    for index, event in enumerate(model_events):
        if not isinstance(event, Mapping):
            errors.append(f"model_event_{index}_must_be_object")
            continue
        usage = event.get("usage")
        observed = _normalize_usage(usage, f"model_event_{index}_usage", errors)
        if observed is None:
            continue
        count += 1
        for field in REQUIRED_TOKEN_FIELDS:
            total[field] = int(total[field]) + int(observed[field])
        if observed["reasoning_tokens"] is None:
            reasoning_observed = False
        elif reasoning_observed:
            total["reasoning_tokens"] = int(total["reasoning_tokens"]) + int(
                observed["reasoning_tokens"]
            )
    if not model_events:
        errors.append("model_events_empty")
    if count != len(model_events):
        return None, count
    if not reasoning_observed:
        total["reasoning_tokens"] = None
    return total, count


def _provider_total(usage: Mapping[str, int | None]) -> int:
    return sum(int(usage[field]) for field in REQUIRED_TOKEN_FIELDS)


def _usage_evidence(
    usage: Mapping[str, int | None] | None,
) -> dict[str, Any] | None:
    if usage is None:
        return None
    reasoning_observed = usage["reasoning_tokens"] is not None
    return {
        **usage,
        "provider_total_tokens": _provider_total(usage),
        "reasoning_tokens_observed": reasoning_observed,
        "unobserved_fields": [] if reasoning_observed else ["reasoning_tokens"],
    }


def _usage_difference(
    whole: Mapping[str, int | None],
    observed: Mapping[str, int | None],
) -> dict[str, Any]:
    difference: dict[str, int | None] = {
        field: int(whole[field]) - int(observed[field])
        for field in REQUIRED_TOKEN_FIELDS
    }
    difference["reasoning_tokens"] = (
        int(whole["reasoning_tokens"]) - int(observed["reasoning_tokens"])
        if whole["reasoning_tokens"] is not None
        and observed["reasoning_tokens"] is not None
        else None
    )
    return _usage_evidence(difference) or {}


def _invalid_receipt(
    *,
    model_invoked: bool,
    source: str,
    errors: Sequence[str],
    cli_turns: int | None,
    provider_responses: int | None,
    provider_requests: int | None,
    top_level_usage: Mapping[str, int | None] | None = None,
    model_events_usage: Mapping[str, int | None] | None = None,
) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "valid": False,
        "errors": list(dict.fromkeys(errors)),
        "source": source,
        "model_invoked": model_invoked,
        **{field: None for field in TOKEN_FIELDS},
        "provider_total_tokens": None,
        "reasoning_tokens_observed": False,
        "unobserved_fields": ["reasoning_tokens"],
        "top_level_usage": _usage_evidence(top_level_usage),
        "model_events_usage": _usage_evidence(model_events_usage),
        "auxiliary_cli_usage": None,
        "cli_turns": cli_turns,
        "provider_responses": provider_responses,
        "provider_responses_scope": "agent_transcript_only",
        "provider_requests": provider_requests,
    }


def parse_claude_usage(
    raw_result: Mapping[str, Any],
    model_events: Sequence[Mapping[str, Any]] | None = None,
    model_invoked: bool = True,
    *,
    provider_responses: int | None = None,
    provider_requests: int | None = None,
) -> dict[str, Any]:
    """Normalize complete Claude usage or raise :class:`ProviderUsageError`.

    ``num_turns`` is a CLI counter, not a provider-request count.  It remains
    nullable when absent.  Provider response/request counts are independent
    observed inputs and likewise remain nullable rather than being inferred.
    """
    if not isinstance(raw_result, Mapping):
        receipt = _invalid_receipt(
            model_invoked=model_invoked,
            source="unavailable",
            errors=["raw_result_must_be_object"],
            cli_turns=None,
            provider_responses=None,
            provider_requests=None,
        )
        raise ProviderUsageError(receipt["errors"], receipt)
    if type(model_invoked) is not bool:
        receipt = _invalid_receipt(
            model_invoked=True,
            source="unavailable",
            errors=["model_invoked_must_be_boolean"],
            cli_turns=None,
            provider_responses=None,
            provider_requests=None,
        )
        raise ProviderUsageError(receipt["errors"], receipt)

    counter_errors: list[str] = []
    cli_turns = _optional_counter(
        raw_result.get("num_turns"), "cli_turns", counter_errors
    )
    provider_responses = _optional_counter(
        provider_responses, "provider_responses", counter_errors
    )
    provider_requests = _optional_counter(
        provider_requests, "provider_requests", counter_errors
    )

    if not model_invoked:
        errors = list(counter_errors)
        if cli_turns not in (None, 0):
            errors.append("cli_turns_present_without_model")
        if provider_responses not in (None, 0):
            errors.append("provider_responses_present_without_model")
        if provider_requests not in (None, 0):
            errors.append("provider_requests_present_without_model")
        if any(key in raw_result for key in ("usage", "modelUsage")):
            errors.append("provider_usage_present_without_model")
        if model_events not in (None, [], ()):
            errors.append("model_events_present_without_model")
        if errors:
            receipt = _invalid_receipt(
                model_invoked=False,
                source="model_not_invoked",
                errors=errors,
                cli_turns=cli_turns,
                provider_responses=provider_responses,
                provider_requests=provider_requests,
            )
            raise ProviderUsageError(receipt["errors"], receipt)
        return {
            "schema_version": SCHEMA_VERSION,
            "valid": True,
            "errors": [],
            "source": "model_not_invoked",
            "model_invoked": False,
            **{field: None for field in TOKEN_FIELDS},
            "provider_total_tokens": None,
            "reasoning_tokens_observed": False,
            "unobserved_fields": ["reasoning_tokens"],
            "top_level_usage": None,
            "model_events_usage": None,
            "auxiliary_cli_usage": None,
            "cli_turns": cli_turns,
            "provider_responses": provider_responses,
            "provider_responses_scope": "agent_transcript_only",
            "provider_requests": provider_requests,
        }

    errors = list(counter_errors)
    if cli_turns == 0:
        errors.append("cli_turns_must_be_positive_when_model_invoked")
    if provider_responses == 0:
        errors.append("provider_responses_must_be_positive_when_model_invoked")
    if provider_requests == 0:
        errors.append("provider_requests_must_be_positive_when_model_invoked")
    if (
        provider_responses is not None
        and provider_requests is not None
        and provider_responses > provider_requests
    ):
        errors.append("provider_responses_cannot_exceed_provider_requests")

    if "modelUsage" not in raw_result:
        errors.append("modelUsage_missing")
        normalized = None
    else:
        normalized = _model_usage_total(raw_result.get("modelUsage"), errors)
    if "usage" not in raw_result:
        errors.append("top_level_usage_missing")
        top_level_usage = None
    else:
        top_level_usage = _normalize_usage(
            raw_result.get("usage"), "top_level_usage", errors
        )

    event_total, observed_event_count = _event_usage_total(model_events, errors)
    if event_total is not None:
        if provider_responses is not None and provider_responses != observed_event_count:
            errors.append("provider_responses_inconsistent_with_model_events")
        if normalized is not None:
            for field in REQUIRED_TOKEN_FIELDS:
                if int(event_total[field]) > int(normalized[field]):
                    errors.append(
                        "provider_usage_exceeds_whole_invocation:"
                        f"model_events:{field}"
                    )
            if (
                normalized["reasoning_tokens"] is not None
                and event_total["reasoning_tokens"] is not None
                and int(event_total["reasoning_tokens"])
                > int(normalized["reasoning_tokens"])
            ):
                errors.append(
                    "provider_usage_exceeds_whole_invocation:"
                    "model_events:reasoning_tokens"
                )
    if normalized is None:
        errors.append("missing_complete_provider_usage")

    provider_total = _provider_total(normalized) if normalized is not None else None
    if provider_total == 0:
        errors.append("provider_total_tokens_must_be_positive")

    source = "whole_invocation_model_usage" if normalized is not None else "unavailable"
    if errors or normalized is None or provider_total is None:
        receipt = _invalid_receipt(
            model_invoked=True,
            source=source,
            errors=errors,
            cli_turns=cli_turns,
            provider_responses=provider_responses,
            provider_requests=provider_requests,
            top_level_usage=top_level_usage,
            model_events_usage=event_total,
        )
        raise ProviderUsageError(receipt["errors"], receipt)
    reasoning_observed = normalized["reasoning_tokens"] is not None
    return {
        "schema_version": SCHEMA_VERSION,
        "valid": True,
        "errors": [],
        "source": source,
        "model_invoked": True,
        **normalized,
        "provider_total_tokens": provider_total,
        "reasoning_tokens_observed": reasoning_observed,
        "unobserved_fields": [] if reasoning_observed else ["reasoning_tokens"],
        "top_level_usage": _usage_evidence(top_level_usage),
        "model_events_usage": _usage_evidence(event_total),
        "auxiliary_cli_usage": (
            _usage_difference(normalized, event_total)
            if event_total is not None
            else None
        ),
        "cli_turns": cli_turns,
        "provider_responses": provider_responses,
        "provider_responses_scope": "agent_transcript_only",
        "provider_requests": provider_requests,
    }
