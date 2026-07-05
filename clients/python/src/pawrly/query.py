"""Streaming query result."""

from collections.abc import Iterator

from .result import QueryResult


class QueryHandle:
    """A streaming query result. Iterate the rows (memory-bounded), or
    :meth:`collect` them into a :class:`QueryResult`. ``id`` is the server query
    id for :meth:`PawrlyClient.cancel` — populated over gRPC, empty over REST.
    """

    def __init__(self, id: str, rows: Iterator[dict], meta: dict) -> None:
        self.id = id
        self._rows = rows
        self._meta = meta

    def __iter__(self) -> Iterator[dict]:
        return iter(self._rows)

    def collect(self) -> QueryResult:
        rows = list(self._rows)
        return QueryResult(
            columns=self._meta["columns"],
            rows=rows,
            row_count=self._meta["row_count"],
            truncated=self._meta["truncated"],
        )
