/** A Pawrly error carrying a stable `PAWRLY_*` code, uniform across transports. */
export class PawrlyError extends Error {
  readonly code: string;
  readonly hint?: string;

  constructor(code: string, message: string, hint?: string) {
    super(message);
    this.name = "PawrlyError";
    this.code = code;
    this.hint = hint;
  }
}

/** Raised when a method isn't available over the chosen transport. */
export class UnsupportedByTransport extends PawrlyError {
  constructor(method: string, transport: string) {
    super(
      "PAWRLY_UNSUPPORTED",
      `\`${method}\` is not supported over the ${transport} transport`,
    );
    this.name = "UnsupportedByTransport";
  }
}
