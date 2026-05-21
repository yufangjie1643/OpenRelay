#!/usr/bin/env python3
"""
Diagnose why recorded cache hit rate is low for kimi-for-coding.

The script streams usage.jsonl, scans conversation files one at a time, and
prints only aggregate metadata. It never prints request or response text.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import hashlib
import json
import math
import os
import re
import sys
from pathlib import Path
from typing import Any, Iterable


DEFAULT_MODEL = "kimi-for-coding"
SMALL_PROMPT_TOKENS = 1024
LONG_PROMPT_TOKENS = 4096


@dataclasses.dataclass
class Finding:
    code: str
    title: str
    detail: str
    impact: str = ""


@dataclasses.dataclass
class ExtractedUsage:
    input_tokens: int = 0
    cached_tokens: int = 0
    cached_write_tokens: int = 0
    output_tokens: int = 0

    @property
    def has_tokens(self) -> bool:
        return (
            self.input_tokens > 0
            or self.cached_tokens > 0
            or self.cached_write_tokens > 0
            or self.output_tokens > 0
        )

    @property
    def cache_hit_rate(self) -> float:
        if self.input_tokens <= 0:
            return 0.0
        return self.cached_tokens / self.input_tokens


@dataclasses.dataclass
class BucketStats:
    requests: int = 0
    ok_requests: int = 0
    error_requests: int = 0
    input_tokens: int = 0
    cached_tokens: int = 0
    cached_write_tokens: int = 0
    output_tokens: int = 0
    cost: float = 0.0
    duration_ms: int = 0
    zero_cache_requests: int = 0
    long_prompt_requests: int = 0
    long_prompt_zero_cache_requests: int = 0
    small_prompt_requests: int = 0

    def add(self, row: dict[str, Any]) -> None:
        input_tokens = as_int(row.get("input_tokens"))
        cached_tokens = as_int(row.get("cached_tokens"))
        status = as_int(row.get("status"))

        self.requests += 1
        self.input_tokens += input_tokens
        self.cached_tokens += cached_tokens
        self.cached_write_tokens += as_int(row.get("cached_write_tokens"))
        self.output_tokens += as_int(row.get("output_tokens"))
        self.cost += as_float(row.get("cost"))
        self.duration_ms += as_int(row.get("duration_ms"))

        if 200 <= status < 300:
            self.ok_requests += 1
        else:
            self.error_requests += 1
        if input_tokens > 0 and cached_tokens == 0:
            self.zero_cache_requests += 1
        if input_tokens >= LONG_PROMPT_TOKENS:
            self.long_prompt_requests += 1
            if cached_tokens == 0:
                self.long_prompt_zero_cache_requests += 1
        if 0 < input_tokens < SMALL_PROMPT_TOKENS:
            self.small_prompt_requests += 1

    @property
    def cache_hit_rate(self) -> float:
        if self.input_tokens <= 0:
            return 0.0
        return self.cached_tokens / self.input_tokens

    @property
    def avg_input_tokens(self) -> float:
        if self.requests <= 0:
            return 0.0
        return self.input_tokens / self.requests

    @property
    def avg_duration_ms(self) -> float:
        if self.requests <= 0:
            return 0.0
        return self.duration_ms / self.requests


@dataclasses.dataclass
class UsageSummary:
    model: str
    usage_path: Path
    rows_scanned: int
    rows_parse_errors: int
    first_timestamp: int | None
    last_timestamp: int | None
    total: BucketStats
    stream: BucketStats
    non_stream: BucketStats
    by_status: dict[str, BucketStats]
    by_key: dict[str, BucketStats]
    by_user_agent: dict[str, BucketStats]
    primary_findings: list[Finding]


@dataclasses.dataclass
class ConversationSummary:
    conversation_dir: Path
    files_scanned: int = 0
    matching_files: int = 0
    parse_errors: int = 0
    stream_inputs: int = 0
    output_raw_files: int = 0
    output_usage_files: int = 0
    output_raw_usage_files: int = 0
    output_raw_cached_files: int = 0
    output_raw_input_tokens: int = 0
    output_raw_cached_tokens: int = 0
    output_raw_cached_write_tokens: int = 0
    output_raw_output_tokens: int = 0
    messages_total: int = 0
    duplicate_full_inputs: int = 0
    duplicate_prefix_inputs: int = 0
    unique_full_input_hashes: int = 0
    unique_prefix_hashes: int = 0
    user_agent_counts: collections.Counter[str] = dataclasses.field(default_factory=collections.Counter)
    key_counts: collections.Counter[str] = dataclasses.field(default_factory=collections.Counter)
    prompt_shape_counts: collections.Counter[str] = dataclasses.field(default_factory=collections.Counter)
    sampled_request_ids: list[str] = dataclasses.field(default_factory=list)


@dataclasses.dataclass
class ServerInspection:
    server_path: Path
    found_stream_zero_cache_pattern: bool = False
    found_non_stream_usage_extraction: bool = False


def as_int(value: Any) -> int:
    if value is None or value is False:
        return 0
    try:
        if isinstance(value, bool):
            return int(value)
        return int(value)
    except (TypeError, ValueError):
        return 0


def as_float(value: Any) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def as_bool(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes", "on"}
    return bool(value)


def first_int(*values: Any) -> int:
    for value in values:
        if value is None:
            continue
        try:
            return int(value)
        except (TypeError, ValueError):
            continue
    return 0


def extract_usage_tokens(data: dict[str, Any] | None, fallback_input: int = 0) -> ExtractedUsage:
    if not isinstance(data, dict):
        return ExtractedUsage()
    usage = data.get("usage")
    if not isinstance(usage, dict):
        return ExtractedUsage()

    prompt_details = usage.get("prompt_tokens_details")
    if not isinstance(prompt_details, dict):
        prompt_details = {}
    input_details = usage.get("input_tokens_details")
    if not isinstance(input_details, dict):
        input_details = {}

    input_tokens = first_int(
        usage.get("prompt_tokens"),
        usage.get("input_tokens"),
        usage.get("total_tokens"),
        fallback_input,
    )
    cached_tokens = first_int(
        prompt_details.get("cached_tokens"),
        input_details.get("cached_tokens"),
        usage.get("cached_tokens"),
        usage.get("cache_read_input_tokens"),
        usage.get("cache_hit_tokens"),
    )
    cached_write_tokens = first_int(
        prompt_details.get("cache_write_tokens"),
        input_details.get("cache_write_tokens"),
        usage.get("cache_write_tokens"),
        usage.get("cache_creation_input_tokens"),
    )
    output_tokens = first_int(usage.get("completion_tokens"), usage.get("output_tokens"))
    return ExtractedUsage(
        input_tokens=input_tokens,
        cached_tokens=cached_tokens,
        cached_write_tokens=cached_write_tokens,
        output_tokens=output_tokens,
    )


def iter_sse_events(raw: str) -> Iterable[dict[str, Any]]:
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        payload = line[5:].strip()
        if not payload or payload == "[DONE]":
            continue
        try:
            event = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict):
            yield event


def extract_sse_usage(raw: str) -> ExtractedUsage | None:
    last_usage: ExtractedUsage | None = None
    for event in iter_sse_events(raw):
        usage = extract_usage_tokens(event)
        if usage.has_tokens:
            last_usage = usage
    return last_usage


def pct(value: float) -> str:
    if not math.isfinite(value):
        return "0.00%"
    return f"{value * 100:.2f}%"


def short_user_agent(value: Any) -> str:
    text = str(value or "").strip()
    if not text:
        return "(blank)"
    return re.sub(r"\s+", " ", text)[:120]


def safe_key_name(row: dict[str, Any]) -> str:
    key_name = str(row.get("key_name") or "").strip()
    if key_name:
        return key_name
    if row.get("api_key"):
        return "(api key present, name missing)"
    return "(unknown)"


def iter_jsonl(path: Path) -> Iterable[tuple[int, dict[str, Any] | None]]:
    with path.open("r", encoding="utf-8", errors="replace") as handle:
        for line_number, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                yield line_number, None
                continue
            if isinstance(value, dict):
                yield line_number, value
            else:
                yield line_number, None


def add_to_bucket(mapping: dict[str, BucketStats], key: str, row: dict[str, Any]) -> None:
    if key not in mapping:
        mapping[key] = BucketStats()
    mapping[key].add(row)


def analyze_usage(usage_path: Path | str, model: str = DEFAULT_MODEL) -> UsageSummary:
    usage_path = Path(usage_path)
    total = BucketStats()
    stream = BucketStats()
    non_stream = BucketStats()
    by_status: dict[str, BucketStats] = {}
    by_key: dict[str, BucketStats] = {}
    by_user_agent: dict[str, BucketStats] = {}
    rows_scanned = 0
    rows_parse_errors = 0
    first_timestamp: int | None = None
    last_timestamp: int | None = None

    if not usage_path.exists():
        return UsageSummary(
            model=model,
            usage_path=usage_path,
            rows_scanned=0,
            rows_parse_errors=0,
            first_timestamp=None,
            last_timestamp=None,
            total=total,
            stream=stream,
            non_stream=non_stream,
            by_status=by_status,
            by_key=by_key,
            by_user_agent=by_user_agent,
            primary_findings=[
                Finding("usage_file_missing", "usage.jsonl was not found", str(usage_path)),
            ],
        )

    for _, row in iter_jsonl(usage_path):
        rows_scanned += 1
        if row is None:
            rows_parse_errors += 1
            continue
        if row.get("model") != model:
            continue

        ts = as_int(row.get("timestamp"))
        if ts:
            first_timestamp = ts if first_timestamp is None else min(first_timestamp, ts)
            last_timestamp = ts if last_timestamp is None else max(last_timestamp, ts)

        total.add(row)
        if as_bool(row.get("stream")):
            stream.add(row)
        else:
            non_stream.add(row)
        add_to_bucket(by_status, str(row.get("status", "(missing)")), row)
        add_to_bucket(by_key, safe_key_name(row), row)
        add_to_bucket(by_user_agent, short_user_agent(row.get("user_agent")), row)

    findings = make_usage_findings(total, stream, non_stream)
    return UsageSummary(
        model=model,
        usage_path=usage_path,
        rows_scanned=rows_scanned,
        rows_parse_errors=rows_parse_errors,
        first_timestamp=first_timestamp,
        last_timestamp=last_timestamp,
        total=total,
        stream=stream,
        non_stream=non_stream,
        by_status=by_status,
        by_key=by_key,
        by_user_agent=by_user_agent,
        primary_findings=findings,
    )


def make_usage_findings(total: BucketStats, stream: BucketStats, non_stream: BucketStats) -> list[Finding]:
    findings: list[Finding] = []
    if total.requests == 0:
        return [Finding("no_model_usage", "No usage rows for target model", "Check model alias spelling.")]

    stream_request_share = stream.requests / total.requests if total.requests else 0.0
    stream_token_share = stream.input_tokens / total.input_tokens if total.input_tokens else 0.0
    if stream.requests and stream.cached_tokens == 0 and (stream_request_share >= 0.2 or stream_token_share >= 0.2):
        findings.append(
            Finding(
                "streaming_usage_not_recorded",
                "Streaming requests record cached_tokens as zero",
                (
                    f"{stream.requests}/{total.requests} requests are stream=true "
                    f"({pct(stream_request_share)} of requests, {pct(stream_token_share)} of input tokens). "
                    "In this proxy, streaming rows are logged with cached_tokens=0, so the displayed hit rate is biased downward."
                ),
                "Recorded cache hit rate is not a reliable measure for streaming traffic.",
            )
        )

    if non_stream.requests:
        if non_stream.cache_hit_rate >= 0.5 and total.cache_hit_rate < non_stream.cache_hit_rate * 0.7:
            findings.append(
                Finding(
                    "non_stream_cache_is_higher",
                    "Non-streaming traffic shows a much higher hit rate",
                    (
                        f"non-stream hit rate is {pct(non_stream.cache_hit_rate)} "
                        f"vs total {pct(total.cache_hit_rate)}."
                    ),
                    "The apparent low total rate is likely dominated by stream accounting.",
                )
            )
        elif non_stream.cache_hit_rate < 0.1 and non_stream.input_tokens >= LONG_PROMPT_TOKENS:
            findings.append(
                Finding(
                    "non_stream_cache_also_low",
                    "Non-streaming traffic also has low cache hits",
                    (
                        f"non-stream hit rate is {pct(non_stream.cache_hit_rate)} across "
                        f"{non_stream.input_tokens:,} input tokens."
                    ),
                    "Prompt prefixes may vary too much, or the upstream service may not be returning cache usage fields.",
                )
            )
    else:
        findings.append(
            Finding(
                "no_non_stream_baseline",
                "No non-streaming baseline exists for this model",
                "All target-model usage rows are stream=true.",
                "Run a small non-stream request pair to confirm upstream cache reporting.",
            )
        )

    if total.error_requests:
        findings.append(
            Finding(
                "errors_included_in_rate",
                "Failed or aborted requests are included in totals",
                f"{total.error_requests}/{total.requests} rows are non-2xx.",
                "Errors add input tokens and zero cached tokens, lowering the aggregate rate.",
            )
        )

    if total.long_prompt_requests and total.long_prompt_zero_cache_requests / total.long_prompt_requests >= 0.5:
        findings.append(
            Finding(
                "long_prompts_zero_cache",
                "Many long prompts have zero recorded cache",
                (
                    f"{total.long_prompt_zero_cache_requests}/{total.long_prompt_requests} requests with "
                    f">={LONG_PROMPT_TOKENS} input tokens recorded zero cached tokens."
                ),
                "This is expected for stream accounting, but suspicious for confirmed non-stream traffic.",
            )
        )

    if total.small_prompt_requests / total.requests >= 0.5:
        findings.append(
            Finding(
                "many_small_prompts",
                "Many requests are too small to benefit much from caching",
                f"{total.small_prompt_requests}/{total.requests} rows have <{SMALL_PROMPT_TOKENS} input tokens.",
                "Small probes and health checks dilute the aggregate cache rate.",
            )
        )

    return findings or [
        Finding(
            "no_clear_usage_cause",
            "No single dominant usage-level cause found",
            "Inspect conversation prefix reuse and upstream cache support.",
        )
    ]


def stable_hash(value: Any) -> str:
    dumped = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(dumped.encode("utf-8", errors="replace")).hexdigest()


def message_shape(messages: Any) -> str:
    if not isinstance(messages, list):
        return "messages:missing"
    roles = []
    for message in messages[:8]:
        if isinstance(message, dict):
            roles.append(str(message.get("role") or "?"))
        else:
            roles.append(type(message).__name__)
    suffix = "+" if len(messages) > len(roles) else ""
    return f"{len(messages)}:" + ",".join(roles) + suffix


def has_usage_object(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    usage = value.get("usage")
    return isinstance(usage, dict) and bool(usage)


def analyze_conversations(
    conversation_dir: Path | str,
    model: str = DEFAULT_MODEL,
    prefix_messages: int = 3,
    max_files: int | None = None,
    sample_request_ids: int = 5,
) -> ConversationSummary:
    conversation_dir = Path(conversation_dir)
    summary = ConversationSummary(conversation_dir=conversation_dir)
    full_hash_counts: collections.Counter[str] = collections.Counter()
    prefix_hash_counts: collections.Counter[str] = collections.Counter()

    if not conversation_dir.exists():
        return summary

    for path in sorted(conversation_dir.glob("*.json")):
        if max_files is not None and summary.files_scanned >= max_files:
            break
        summary.files_scanned += 1
        try:
            data = json.loads(path.read_text(encoding="utf-8", errors="replace"))
        except (OSError, json.JSONDecodeError):
            summary.parse_errors += 1
            continue
        if not isinstance(data, dict) or data.get("model") != model:
            continue

        summary.matching_files += 1
        request_id = str(data.get("request_id") or "")
        if request_id and len(summary.sampled_request_ids) < sample_request_ids:
            summary.sampled_request_ids.append(request_id)

        input_body = data.get("input") if isinstance(data.get("input"), dict) else {}
        output_body = data.get("output") if isinstance(data.get("output"), dict) else None
        output_raw = data.get("output_raw")
        messages = input_body.get("messages") if isinstance(input_body, dict) else None

        if as_bool(input_body.get("stream")):
            summary.stream_inputs += 1
        if isinstance(output_raw, str):
            summary.output_raw_files += 1
            usage = extract_sse_usage(output_raw)
            if usage is not None:
                summary.output_raw_usage_files += 1
                summary.output_raw_input_tokens += usage.input_tokens
                summary.output_raw_cached_tokens += usage.cached_tokens
                summary.output_raw_cached_write_tokens += usage.cached_write_tokens
                summary.output_raw_output_tokens += usage.output_tokens
                if usage.cached_tokens > 0:
                    summary.output_raw_cached_files += 1
        if output_body is not None and has_usage_object(output_body):
            summary.output_usage_files += 1

        if isinstance(messages, list):
            summary.messages_total += len(messages)
            full_hash_counts[stable_hash(messages)] += 1
            prefix_hash_counts[stable_hash(messages[:prefix_messages])] += 1
        else:
            full_hash_counts[stable_hash(input_body)] += 1
            prefix_hash_counts[stable_hash(input_body)] += 1

        summary.user_agent_counts[short_user_agent(data.get("user_agent"))] += 1
        summary.key_counts[str(data.get("key_name") or "(unknown)")] += 1
        summary.prompt_shape_counts[message_shape(messages)] += 1

    summary.unique_full_input_hashes = len(full_hash_counts)
    summary.unique_prefix_hashes = len(prefix_hash_counts)
    summary.duplicate_full_inputs = sum(count - 1 for count in full_hash_counts.values() if count > 1)
    summary.duplicate_prefix_inputs = sum(count - 1 for count in prefix_hash_counts.values() if count > 1)
    return summary


def inspect_server(server_path: Path | str) -> ServerInspection:
    server_path = Path(server_path)
    result = ServerInspection(server_path=server_path)
    try:
        text = server_path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return result
    compact = re.sub(r"\s+", " ", text)
    result.found_non_stream_usage_extraction = "extractUsageTokens(data" in text
    result.found_stream_zero_cache_pattern = bool(
        re.search(r"stream:\s*true[^}]+cached_tokens:\s*0", compact)
        or re.search(r"cached_tokens:\s*0[^}]+stream:\s*true", compact)
    )
    return result


def top_rows(mapping: dict[str, BucketStats], limit: int) -> list[tuple[str, BucketStats]]:
    return sorted(mapping.items(), key=lambda item: item[1].input_tokens, reverse=True)[:limit]


def print_usage_report(summary: UsageSummary, top_limit: int) -> None:
    print(f"# Cache hit diagnosis for {summary.model}")
    print()
    print(f"Usage file: {summary.usage_path}")
    print(f"Rows scanned: {summary.rows_scanned:,}; parse errors: {summary.rows_parse_errors:,}")
    if summary.first_timestamp and summary.last_timestamp:
        print(f"Timestamp range: {summary.first_timestamp} .. {summary.last_timestamp}")
    print()

    print("## Usage summary")
    print("| bucket | requests | ok | errors | input | cached | hit rate | output | avg input | avg ms |")
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for name, bucket in [
        ("total", summary.total),
        ("stream=true", summary.stream),
        ("stream=false", summary.non_stream),
    ]:
        print(
            f"| {name} | {bucket.requests:,} | {bucket.ok_requests:,} | {bucket.error_requests:,} | "
            f"{bucket.input_tokens:,} | {bucket.cached_tokens:,} | {pct(bucket.cache_hit_rate)} | "
            f"{bucket.output_tokens:,} | {bucket.avg_input_tokens:.1f} | {bucket.avg_duration_ms:.0f} |"
        )
    print()

    print("## Findings")
    for index, finding in enumerate(summary.primary_findings, 1):
        print(f"{index}. {finding.title} [{finding.code}]")
        print(f"   {finding.detail}")
        if finding.impact:
            print(f"   Impact: {finding.impact}")
    print()

    if summary.by_status:
        print("## By status")
        for status, bucket in top_rows(summary.by_status, top_limit):
            print(
                f"- {status}: requests={bucket.requests:,}, input={bucket.input_tokens:,}, "
                f"cached={bucket.cached_tokens:,}, hit={pct(bucket.cache_hit_rate)}"
            )
        print()

    if summary.by_key:
        print("## By key name")
        for key, bucket in top_rows(summary.by_key, top_limit):
            print(
                f"- {key}: requests={bucket.requests:,}, input={bucket.input_tokens:,}, "
                f"cached={bucket.cached_tokens:,}, hit={pct(bucket.cache_hit_rate)}"
            )
        print()

    if summary.by_user_agent:
        print("## By user agent")
        for user_agent, bucket in top_rows(summary.by_user_agent, top_limit):
            print(
                f"- {user_agent}: requests={bucket.requests:,}, input={bucket.input_tokens:,}, "
                f"cached={bucket.cached_tokens:,}, hit={pct(bucket.cache_hit_rate)}"
            )
        print()


def print_conversation_report(summary: ConversationSummary, prefix_messages: int, top_limit: int) -> None:
    print("## Conversation metadata")
    print(f"Conversation dir: {summary.conversation_dir}")
    print(
        f"Files scanned: {summary.files_scanned:,}; matching model files: {summary.matching_files:,}; "
        f"parse errors: {summary.parse_errors:,}"
    )
    if summary.matching_files == 0:
        print()
        return

    avg_messages = summary.messages_total / summary.matching_files if summary.matching_files else 0.0
    full_reuse = summary.duplicate_full_inputs / summary.matching_files if summary.matching_files else 0.0
    prefix_reuse = summary.duplicate_prefix_inputs / summary.matching_files if summary.matching_files else 0.0
    print(
        f"Stream inputs: {summary.stream_inputs:,}; output_raw files: {summary.output_raw_files:,}; "
        f"output.usage files: {summary.output_usage_files:,}; SSE usage files: {summary.output_raw_usage_files:,}"
    )
    if summary.output_raw_usage_files:
        recovered_rate = (
            summary.output_raw_cached_tokens / summary.output_raw_input_tokens
            if summary.output_raw_input_tokens
            else 0.0
        )
        print(
            f"Recovered SSE usage: input={summary.output_raw_input_tokens:,}; "
            f"cached={summary.output_raw_cached_tokens:,}; output={summary.output_raw_output_tokens:,}; "
            f"hit={pct(recovered_rate)}; files with cached tokens={summary.output_raw_cached_files:,}"
        )
    print(
        f"Avg message count: {avg_messages:.1f}; full input duplicate share: {pct(full_reuse)}; "
        f"first {prefix_messages} messages duplicate share: {pct(prefix_reuse)}"
    )
    print(
        f"Unique full input hashes: {summary.unique_full_input_hashes:,}; "
        f"unique prefix hashes: {summary.unique_prefix_hashes:,}"
    )
    if summary.sampled_request_ids:
        print("Sample request ids: " + ", ".join(summary.sampled_request_ids))
    print()

    print("## Conversation prompt shapes")
    for shape, count in summary.prompt_shape_counts.most_common(top_limit):
        print(f"- {shape}: {count:,}")
    print()

    print("## Conversation key names")
    for key, count in summary.key_counts.most_common(top_limit):
        print(f"- {key}: {count:,}")
    print()

    print("## Conversation user agents")
    for user_agent, count in summary.user_agent_counts.most_common(top_limit):
        print(f"- {user_agent}: {count:,}")
    print()

    if summary.output_raw_usage_files and summary.output_raw_cached_tokens > 0:
        print("## Conversation finding")
        print(
            "- Saved Kimi stream output contains structured SSE usage/cache metadata. "
            "The current usage.jsonl stream accounting does not use it, which explains the near-zero recorded hit rate."
        )
        print()
    elif summary.output_raw_files and summary.output_raw_usage_files == 0:
        print("## Conversation finding")
        print(
            "- Saved streaming conversation output_raw does not appear to include usage/cache metadata, "
            "so past stream cache tokens cannot be recovered from conversation logs alone."
        )
        print()


def print_server_report(inspection: ServerInspection) -> None:
    print("## Proxy code inspection")
    print(f"Server file: {inspection.server_path}")
    print(f"Non-stream usage extraction present: {inspection.found_non_stream_usage_extraction}")
    print(f"Streaming cached_tokens=0 logging pattern present: {inspection.found_stream_zero_cache_pattern}")
    if inspection.found_stream_zero_cache_pattern:
        print(
            "Conclusion: the proxy accounting path itself explains a low recorded hit rate when "
            "kimi-for-coding is mostly streamed."
        )
    print()


def build_json_output(
    usage: UsageSummary,
    conversations: ConversationSummary | None,
    server: ServerInspection | None,
) -> dict[str, Any]:
    def bucket_to_dict(bucket: BucketStats) -> dict[str, Any]:
        return dataclasses.asdict(bucket) | {"cache_hit_rate": bucket.cache_hit_rate}

    conversation_data = None
    if conversations is not None:
        conversation_data = dataclasses.asdict(conversations)
        conversation_data["conversation_dir"] = str(conversations.conversation_dir)

    server_data = None
    if server is not None:
        server_data = dataclasses.asdict(server)
        server_data["server_path"] = str(server.server_path)

    return {
        "model": usage.model,
        "usage": {
            "usage_path": str(usage.usage_path),
            "rows_scanned": usage.rows_scanned,
            "rows_parse_errors": usage.rows_parse_errors,
            "total": bucket_to_dict(usage.total),
            "stream": bucket_to_dict(usage.stream),
            "non_stream": bucket_to_dict(usage.non_stream),
            "findings": [dataclasses.asdict(item) for item in usage.primary_findings],
        },
        "conversations": conversation_data,
        "server": server_data,
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Analyze recorded cache hit rate for a proxied model without printing conversation text."
    )
    parser.add_argument("--model", default=DEFAULT_MODEL, help=f"model alias to inspect (default: {DEFAULT_MODEL})")
    parser.add_argument("--usage", type=Path, default=Path("usage.jsonl"), help="path to usage.jsonl")
    parser.add_argument("--conversations", type=Path, default=Path("conversations"), help="conversation log directory")
    parser.add_argument("--server", type=Path, default=Path("web") / "server.js", help="proxy server file")
    parser.add_argument("--skip-conversations", action="store_true", help="skip conversation metadata scan")
    parser.add_argument("--max-conversations", type=int, default=None, help="limit conversation files scanned")
    parser.add_argument("--prefix-messages", type=int, default=3, help="message prefix length used for reuse hashes")
    parser.add_argument("--top", type=int, default=8, help="number of top groups to print")
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON instead of markdown")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(list(sys.argv[1:] if argv is None else argv))
    usage = analyze_usage(args.usage, args.model)
    conversations = None
    if not args.skip_conversations:
        conversations = analyze_conversations(
            args.conversations,
            args.model,
            prefix_messages=max(1, args.prefix_messages),
            max_files=args.max_conversations,
        )
    server = inspect_server(args.server)

    if args.json:
        print(json.dumps(build_json_output(usage, conversations, server), ensure_ascii=False, indent=2))
    else:
        print_usage_report(usage, args.top)
        if conversations is not None:
            print_conversation_report(conversations, max(1, args.prefix_messages), args.top)
        print_server_report(server)

    return 2 if usage.total.requests == 0 else 0


if __name__ == "__main__":
    raise SystemExit(main())
