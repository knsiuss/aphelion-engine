export interface AphelionMiddlewareOptions {
  profile?: "LongLived" | "Ephemeral";
  ffiLibraryPath?: string;
}

export interface SessionHint {
  sessionId?: string;
  model?: string;
}

export interface AphelionMiddleware {
  readonly profile: "LongLived" | "Ephemeral";
  transformRequest(rawBody: string | Uint8Array, sessionHint?: SessionHint): Uint8Array;
  transformSseStream(
    readableStream: ReadableStream<Uint8Array>,
    sessionHint?: SessionHint
  ): ReadableStream<Uint8Array>;
  dispose(sessionId?: string): void;
}

export function createAphelionMiddleware(
  options?: AphelionMiddlewareOptions
): AphelionMiddleware;
