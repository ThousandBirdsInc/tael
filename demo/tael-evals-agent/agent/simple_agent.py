"""Deterministic local-wiki agent for the Tael eval demo.

The original take-home used Claude tool calls against Wikipedia. This demo keeps
the same agent shape, but uses rules and a local corpus so anyone can run it
offline while watching Tael collect eval spans and scores.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from agent.wiki import get_article, search_wiki

try:
    from opentelemetry import trace
    from opentelemetry.context import attach, detach
    from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
    from opentelemetry.sdk.resources import Resource
    from opentelemetry.sdk.trace import TracerProvider
    from opentelemetry.sdk.trace.export import BatchSpanProcessor
    from opentelemetry.trace import (
        NonRecordingSpan,
        SpanContext,
        Status,
        StatusCode,
        TraceFlags,
        TraceState,
        set_span_in_context,
    )
except ModuleNotFoundError as exc:
    raise SystemExit(
        "missing OpenTelemetry demo dependencies; run "
        "`uv run --project demo/tael-evals-agent python agent/simple_agent.py --help`"
    ) from exc


TRACER = trace.get_tracer("tael.demo.agent")


def _eval_attrs() -> dict[str, str]:
    attrs = {
        "tael.eval.suite_id": os.environ.get("TAEL_EVAL_SUITE_ID", ""),
        "tael.eval.run_id": os.environ.get("TAEL_EVAL_RUN_ID", ""),
        "tael.eval.case_id": os.environ.get("TAEL_EVAL_CASE_ID", ""),
        "tael.eval.role": "agent",
    }
    return {key: value for key, value in attrs.items() if value}


def configure_tracing() -> TracerProvider:
    endpoint = os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317")
    provider = TracerProvider(resource=Resource.create({"service.name": "tael-demo-agent"}))
    provider.add_span_processor(
        BatchSpanProcessor(OTLPSpanExporter(endpoint=endpoint, insecure=True))
    )
    trace.set_tracer_provider(provider)
    return provider


def attach_eval_parent():
    trace_id = os.environ.get("TAEL_EVAL_TRACE_ID")
    span_id = os.environ.get("TAEL_EVAL_SPAN_ID")
    if not trace_id or not span_id:
        return None

    context = SpanContext(
        trace_id=int(trace_id, 16),
        span_id=int(span_id, 16),
        is_remote=True,
        trace_flags=TraceFlags(TraceFlags.SAMPLED),
        trace_state=TraceState(),
    )
    return attach(set_span_in_context(NonRecordingSpan(context)))


def _search(query: str, calls: list[dict]) -> list[dict]:
    with TRACER.start_as_current_span(
        "search_wiki", attributes={**_eval_attrs(), "demo.tool.query": query}
    ) as span:
        results = search_wiki(query)
        span.set_attribute("demo.tool.result_count", len(results))
        span.set_attribute("demo.tool.result_titles", ", ".join(r["title"] for r in results))
    calls.append({"name": "search_wiki", "input": {"query": query}, "output": results})
    return results


def answer_question(question: str) -> tuple[str, list[dict]]:
    """Answer by searching the local wiki, sometimes doing a second hop."""
    q = question.lower()
    calls: list[dict] = []

    if "2016 summer olympics" in q and "capital" in q:
        host_hits = _search("2016 Summer Olympics host", calls)
        country = "Brazil" if any("Brazil" in h["snippet"] for h in host_hits) else "unknown"
        _search(f"{country} capital", calls)
        return "The 2016 Summer Olympics were hosted in Brazil, whose capital is Brasilia.", calls

    if "capital of australia" in q:
        _search("capital of Australia", calls)
        return "The capital of Australia is Canberra.", calls

    if "largest city in australia" in q:
        _search("largest city in Australia by population", calls)
        return "The largest city in Australia by population is Sydney.", calls

    if "einstein" in q and "nobel" in q:
        _search("Einstein Nobel Prize photoelectric effect", calls)
        return (
            "Albert Einstein won the 1921 Nobel Prize in Physics for explaining the "
            "photoelectric effect, not for relativity."
        ), calls

    if "benjamin franklin" in q and "president" in q:
        _search("Benjamin Franklin President United States", calls)
        return "Benjamin Franklin never served as President of the United States.", calls

    hits = _search(question, calls)
    if hits:
        article = get_article(hits[0]["title"])
        calls.append({"name": "get_article", "input": {"title": hits[0]["title"]}, "output": article})
        return article, calls
    return "I could not find enough information in the local wiki.", calls


def run_agent(question: str) -> tuple[str, str]:
    started = time.monotonic()
    with TRACER.start_as_current_span(
        "answer_question", attributes={**_eval_attrs(), "demo.question": question}
    ) as span:
        answer, calls = answer_question(question)
        search_count = sum(1 for c in calls if c["name"] == "search_wiki")
        span.set_attribute("demo.answer", answer)
        span.set_attribute("demo.search_count", search_count)
        span.set_attribute("demo.elapsed_seconds", round(time.monotonic() - started, 4))
        span.set_status(Status(StatusCode.OK))
        trace_id = f"{span.get_span_context().trace_id:032x}"
    return answer, trace_id


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--question")
    parser.add_argument("--case-id")
    parser.add_argument("--cases-file")
    args = parser.parse_args()

    question = args.question
    if question is None:
        if not args.case_id or not args.cases_file:
            parser.error("provide --question or --case-id with --cases-file")
        for line in Path(args.cases_file).read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            case = json.loads(line)
            if case.get("case_id") == args.case_id:
                question = case["question"]
                break
        if question is None:
            raise SystemExit(f"case {args.case_id!r} not found in {args.cases_file}")

    provider = configure_tracing()
    token = attach_eval_parent()
    try:
        answer, trace_id = run_agent(question)
        print(json.dumps({"answer": answer, "trace_id": trace_id}))
        provider.force_flush()
        return 0
    finally:
        if token is not None:
            detach(token)
        provider.shutdown()


if __name__ == "__main__":
    raise SystemExit(main())
