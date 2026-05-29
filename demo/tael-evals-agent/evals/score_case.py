"""Score one demo case and emit JSONL consumable by `tael eval score`."""

from __future__ import annotations

import argparse
import json
import time
import urllib.parse
import urllib.request
from pathlib import Path


def _contains_any(text: str, needles: list[str]) -> bool:
    lower = text.lower()
    return any(n.lower() in lower for n in needles)


def _answer_score(case: dict, answer: str) -> tuple[float, list[str]]:
    lower = answer.lower()
    total = 0
    passed = 0
    reasons: list[str] = []

    for needle in case.get("must_include", []):
        total += 1
        if needle.lower() in lower:
            passed += 1
        else:
            reasons.append(f"missing {needle!r}")

    any_needles = case.get("must_include_any", [])
    if any_needles:
        total += 1
        if _contains_any(answer, any_needles):
            passed += 1
        else:
            reasons.append(f"missing any of {any_needles!r}")

    for needle in case.get("must_exclude", []):
        total += 1
        if needle.lower() not in lower:
            passed += 1
        else:
            reasons.append(f"contains forbidden {needle!r}")

    return (1.0 if total == 0 else passed / total), reasons


def _search_score(case: dict, search_count: int) -> tuple[float, list[str]]:
    minimum = int(case.get("min_searches", 1))
    if search_count >= minimum:
        return 1.0, []
    return search_count / minimum, [f"expected at least {minimum} search step(s), got {search_count}"]


def _calibration_score(case: dict, answer: str) -> tuple[float, list[str]]:
    if case.get("category") not in {"false_premise", "unanswerable"}:
        return 1.0, []
    markers = ["not", "never", "no ", "could not", "not for", "not president"]
    if any(m in answer.lower() for m in markers):
        return 1.0, []
    return 0.0, ["expected correction or qualification"]


def _fetch_json(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=5) as response:
        return json.loads(response.read().decode("utf-8"))


def _case_trace(server: str, run_id: str, case_id: str) -> tuple[str, dict]:
    base = server.rstrip("/")
    params = urllib.parse.urlencode(
        [
            ("attribute", f"tael.eval.run_id={run_id}"),
            ("attribute", f"tael.eval.case_id={case_id}"),
            ("last", "1h"),
            ("limit", "200"),
        ]
    )

    for attempt in range(10):
        spans = _fetch_json(f"{base}/api/v1/traces?{params}").get("spans", [])
        answer_spans = [
            span
            for span in spans
            if span.get("operation") == "answer_question"
            and span.get("attributes", {}).get("demo.answer")
        ]
        if answer_spans:
            answer_span = sorted(answer_spans, key=lambda span: span.get("end_time", ""))[-1]
            trace_id = answer_span["trace_id"]
            trace = _fetch_json(f"{base}/api/v1/traces/{trace_id}")
            return trace_id, trace
        time.sleep(0.2 * (attempt + 1))

    raise SystemExit(f"no Tael trace found for run {run_id!r} case {case_id!r}")


def _answer_span(trace_payload: dict) -> dict:
    for span in trace_payload.get("spans", []):
        if span.get("operation") == "answer_question" and span.get("attributes", {}).get(
            "demo.answer"
        ):
            return span
    raise SystemExit("trace did not contain an answer_question span with demo.answer")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--case-json", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--suite-id", required=True)
    parser.add_argument("--server", default="http://127.0.0.1:7701")
    parser.add_argument("--scores-out", default="demo/tael-evals-agent/out/scores.jsonl")
    args = parser.parse_args()

    case = json.loads(args.case_json)
    trace_id, trace = _case_trace(args.server, args.run_id, case["case_id"])
    answer_span = _answer_span(trace)
    attrs = answer_span.get("attributes", {})
    answer = attrs["demo.answer"]

    answer_score, answer_reasons = _answer_score(case, answer)
    search_score, search_reasons = _search_score(case, int(attrs.get("demo.search_count", "0")))
    calibration_score, calibration_reasons = _calibration_score(case, answer)
    overall = (answer_score + search_score + calibration_score) / 3.0
    passed = overall >= 0.8
    rationale = "; ".join(answer_reasons + search_reasons + calibration_reasons) or "passed all deterministic checks"

    common = {
        "suite_id": args.suite_id,
        "run_id": args.run_id,
        "case_id": case["case_id"],
        "trace_id": trace_id,
        "span_id": answer_span.get("span_id"),
        "scorer": "demo-keyword-grader",
        "source": "script",
    }
    rows = [
        {**common, "metric": "answer", "value": answer_score},
        {**common, "metric": "search", "value": search_score},
        {**common, "metric": "calibration", "value": calibration_score},
        {
            **common,
            "metric": "correctness",
            "value": overall,
            "label": "pass" if passed else "fail",
            "rationale": rationale,
        },
    ]

    out = Path(args.scores_out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("a", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row) + "\n")
    print(json.dumps({"case_id": case["case_id"], "overall": overall, "passed": passed}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
