"""Tiny local wiki used by the Tael eval demo.

This mirrors the take-home agent's Wikipedia tool shape but avoids network and
API-key dependencies. The corpus is intentionally small and includes one
multi-hop case so eval progress is visible without being slow.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Article:
    title: str
    text: str
    aliases: tuple[str, ...] = ()


ARTICLES: list[Article] = [
    Article(
        "Australia",
        "Australia is a country and continent in Oceania. Its capital city is Canberra. "
        "Sydney is the largest city in Australia by population.",
        ("australia capital", "largest city australia"),
    ),
    Article(
        "Canberra",
        "Canberra is the capital city of Australia.",
        ("capital of australia",),
    ),
    Article(
        "Sydney",
        "Sydney is the largest city in Australia by population, but it is not the capital.",
        ("largest city in australia",),
    ),
    Article(
        "Albert Einstein",
        "Albert Einstein received the 1921 Nobel Prize in Physics for his explanation of "
        "the photoelectric effect, not for his theory of relativity.",
        ("einstein nobel relativity", "photoelectric effect"),
    ),
    Article(
        "Benjamin Franklin",
        "Benjamin Franklin was a Founding Father of the United States. He never served as "
        "President of the United States.",
        ("franklin president",),
    ),
    Article(
        "2016 Summer Olympics",
        "The 2016 Summer Olympics were hosted by Rio de Janeiro in Brazil.",
        ("2016 summer olympics host",),
    ),
    Article(
        "Brazil",
        "Brazil is a country in South America. Its capital is Brasilia. Rio de Janeiro is "
        "a major city but is not the capital.",
        ("brazil capital", "rio de janeiro country capital"),
    ),
]


def _tokens(text: str) -> set[str]:
    return {
        t.strip(".,:;!?()[]{}\"'").lower()
        for t in text.split()
        if len(t.strip(".,:;!?()[]{}\"'")) >= 3
    }


def search_wiki(query: str, limit: int = 3) -> list[dict]:
    """Return ranked article snippets for a query."""
    q_tokens = _tokens(query)
    ranked: list[tuple[int, Article]] = []
    for article in ARTICLES:
        haystack = " ".join((article.title, article.text, " ".join(article.aliases)))
        score = len(q_tokens & _tokens(haystack))
        if score:
            ranked.append((score, article))
    ranked.sort(key=lambda item: (-item[0], item[1].title))
    return [
        {
            "title": article.title,
            "snippet": article.text[:220],
            "score": score,
        }
        for score, article in ranked[:limit]
    ]


def get_article(title: str) -> str:
    """Return a full article body by exact title."""
    for article in ARTICLES:
        if article.title.lower() == title.lower():
            return article.text
    return f"No article found for {title!r}."
