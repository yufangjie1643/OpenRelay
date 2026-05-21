import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import analyze_kimi_cache as analyzer


class AnalyzeKimiCacheTests(unittest.TestCase):
  def test_usage_summary_splits_streaming_from_non_streaming(self):
    rows = [
      {
        "request_id": "s1",
        "timestamp": 1,
        "model": "kimi-for-coding",
        "input_tokens": 100,
        "cached_tokens": 0,
        "output_tokens": 10,
        "status": 200,
        "stream": True,
      },
      {
        "request_id": "n1",
        "timestamp": 2,
        "model": "kimi-for-coding",
        "input_tokens": 100,
        "cached_tokens": 80,
        "output_tokens": 10,
        "status": 200,
        "stream": False,
      },
      {
        "request_id": "x1",
        "timestamp": 3,
        "model": "other-model",
        "input_tokens": 100,
        "cached_tokens": 100,
        "output_tokens": 10,
        "status": 200,
        "stream": False,
      },
    ]

    with tempfile.TemporaryDirectory() as tmp:
      usage_path = Path(tmp) / "usage.jsonl"
      usage_path.write_text("\n".join(json.dumps(row) for row in rows), encoding="utf-8")

      summary = analyzer.analyze_usage(usage_path, "kimi-for-coding")

    self.assertEqual(summary.total.requests, 2)
    self.assertEqual(summary.stream.requests, 1)
    self.assertEqual(summary.non_stream.requests, 1)
    self.assertAlmostEqual(summary.total.cache_hit_rate, 0.4)
    self.assertAlmostEqual(summary.stream.cache_hit_rate, 0.0)
    self.assertAlmostEqual(summary.non_stream.cache_hit_rate, 0.8)
    self.assertEqual(summary.primary_findings[0].code, "streaming_usage_not_recorded")

  def test_extracts_usage_from_sse_stream_event(self):
    raw = "\n".join([
      'data: {"choices":[{"delta":{"content":"hidden"}}]}',
      'data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,'
      '"cached_tokens":80,"prompt_tokens_details":{"cached_tokens":80}}}',
      "data: [DONE]",
    ])

    usage = analyzer.extract_sse_usage(raw)

    self.assertIsNotNone(usage)
    self.assertEqual(usage.input_tokens, 100)
    self.assertEqual(usage.cached_tokens, 80)
    self.assertEqual(usage.output_tokens, 7)


if __name__ == "__main__":
  unittest.main()
