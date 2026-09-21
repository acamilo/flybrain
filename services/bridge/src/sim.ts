/**
 * Typed client for flysim's localhost control API (`docs/control-api.md`).
 *
 * Every method returns a discriminated `SimResult` rather than throwing, so callers (chat
 * commands, redemption handling, `/health`) can handle "the sim said no" and "the sim didn't
 * answer" as ordinary control flow instead of catch blocks. Timeouts use `AbortSignal.timeout`;
 * the request/response shapes are imported from `@flybrain/feed`, never redeclared.
 */
import type {
  ChatRequest,
  ChatResponse,
  ErrorResponse,
  EventsResponse,
  HealthzResponse,
  StatusResponse,
  StimulateRateLimitedResponse,
  StimulateRequest,
  StimulateResponse,
} from '@flybrain/feed';

export type SimResult<T> =
  | { ok: true; data: T }
  | { ok: false; kind: 'rate_limited'; retryAfterMs: number }
  | { ok: false; kind: 'forbidden'; error: string }
  | { ok: false; kind: 'timeout' }
  | { ok: false; kind: 'http_error'; status: number; error?: string }
  | { ok: false; kind: 'network_error'; error: string };

export interface SimClientOptions {
  baseUrl: string;
  timeoutMs: number;
  /** Injectable for tests; defaults to the global `fetch`. */
  fetchImpl?: typeof fetch;
}

/** Client interface (rather than a concrete class) so tests can substitute a fake without HTTP. */
export interface SimClient {
  status(): Promise<SimResult<StatusResponse>>;
  stimulate(request: StimulateRequest): Promise<SimResult<StimulateResponse>>;
  /**
   * `POST /chat`: one line for the on-screen chat ring. The 422 a refused line gets arrives as
   * `{ kind: 'http_error', status: 422 }`, which is the honest mapping — it is the service saying
   * no, not a transport failure. See `src/onscreen-chat.ts`, the only caller.
   */
  chat(request: ChatRequest): Promise<SimResult<ChatResponse>>;
  events(since?: number, limit?: number): Promise<SimResult<EventsResponse>>;
  healthz(): Promise<SimResult<HealthzResponse>>;
}

export class HttpSimClient implements SimClient {
  private readonly baseUrl: string;
  private readonly timeoutMs: number;
  private readonly fetchImpl: typeof fetch;

  constructor(options: SimClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, '');
    this.timeoutMs = options.timeoutMs;
    this.fetchImpl = options.fetchImpl ?? fetch;
  }

  async status(): Promise<SimResult<StatusResponse>> {
    return this.request<StatusResponse>('GET', '/status');
  }

  async stimulate(request: StimulateRequest): Promise<SimResult<StimulateResponse>> {
    return this.request<StimulateResponse>('POST', '/stimulate', request);
  }

  async chat(request: ChatRequest): Promise<SimResult<ChatResponse>> {
    return this.request<ChatResponse>('POST', '/chat', request);
  }

  async events(since?: number, limit?: number): Promise<SimResult<EventsResponse>> {
    const params = new URLSearchParams();
    if (since !== undefined) params.set('since', String(since));
    if (limit !== undefined) params.set('limit', String(limit));
    const query = params.toString();
    return this.request<EventsResponse>('GET', query ? `/events?${query}` : '/events');
  }

  async healthz(): Promise<SimResult<HealthzResponse>> {
    return this.request<HealthzResponse>('GET', '/healthz');
  }

  private async request<T>(method: string, path: string, body?: unknown): Promise<SimResult<T>> {
    let response: Response;
    try {
      response = await this.fetchImpl(`${this.baseUrl}${path}`, {
        method,
        headers: body === undefined ? undefined : { 'content-type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: AbortSignal.timeout(this.timeoutMs),
      });
    } catch (cause) {
      if (cause instanceof Error && (cause.name === 'TimeoutError' || cause.name === 'AbortError')) {
        return { ok: false, kind: 'timeout' };
      }
      return { ok: false, kind: 'network_error', error: (cause as Error).message };
    }

    if (response.status === 429) {
      const body429 = (await safeJson(response)) as Partial<StimulateRateLimitedResponse> | undefined;
      return { ok: false, kind: 'rate_limited', retryAfterMs: body429?.retryAfterMs ?? 0 };
    }

    if (response.status === 403) {
      const body403 = (await safeJson(response)) as Partial<ErrorResponse> | undefined;
      return { ok: false, kind: 'forbidden', error: body403?.error ?? 'forbidden' };
    }

    if (!response.ok) {
      const errorBody = (await safeJson(response)) as Partial<ErrorResponse> | undefined;
      return { ok: false, kind: 'http_error', status: response.status, error: errorBody?.error };
    }

    const data = (await safeJson(response)) as T;
    return { ok: true, data };
  }
}

async function safeJson(response: Response): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    return undefined;
  }
}
