"""Error types, uniform across transports."""


class PawrlyError(Exception):
    """A Pawrly error carrying a stable ``PAWRLY_*`` code."""

    def __init__(self, code: str, message: str, hint: str | None = None) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.hint = hint


class UnsupportedByTransport(PawrlyError):
    """A method that isn't available over the chosen transport."""

    def __init__(self, method: str, transport: str) -> None:
        super().__init__(
            "PAWRLY_UNSUPPORTED",
            f"`{method}` is not supported over the {transport} transport",
        )
