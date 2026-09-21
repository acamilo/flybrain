/**
 * Shared test doubles: `FakeChatSender` (implements `ChatSender`, never touches twurple) and
 * `FakeSimClient` (implements `SimClient`, in-memory, scriptable via mutable fields) — used by
 * `commands.test.ts`, `redemptions.test.ts` and `explainer.test.ts` so each doesn't redefine
 * them.
 */
import type {
  ChatRequest,
  EventsResponse,
  HealthzResponse,
  StatusResponse,
  StimulateRequest,
} from '@flybrain/feed';
import type { ChatSender } from '../src/chat';
import type { SimClient, SimResult } from '../src/sim';

export class FakeChatSender implements ChatSender {
  readonly sent: string[] = [];
  async sendMessage(text: string): Promise<void> {
    this.sent.push(text);
  }
}

export function sampleStatusResponse(overrides: Partial<StatusResponse> = {}): StatusResponse {
  return {
    protocol: 1,
    seq: 0,
    wallMs: 0,
    status: 'running',
    realtimeFactor: 1,
    uptimeSeconds: 0,
    runSeconds: 0,
    brainMs: 0,
    frame: 0,
    buttons: 0,
    rates: {},
    populationRate: 0,
    spikeCount: 0,
    learning: { enabled: false, updates: 0, changed: 0, synapses: 0, signal: 0 },
    game: {
      mode: 'OVERWORLD',
      semanticRewards: true,
      map: 1,
      badges: 0,
      uniqueLocations: 3,
      rewardTotal: 0,
      rewardCounts: { story: 0, explore: 0, area: 0, pokedex: 0, trainer: 0, wildwin: 0, badge: 0 },
    },
    milestone: { rank: 2, label: 'Left the bedroom', next: 'Reached Route 1', sinceSeconds: 192, attempts: 0 },
    sugar: { active: false, remainingMs: 0, cooldownMs: 0, lastBy: null, todayCount: 0 },
    version: { kernel: 'x', plasticity: 'x', adapter: 'x', binjgb: 'x', dataset: 'x' },
    checkpoint: { latestWallMs: 0, generation: 0 },
    ...overrides,
  };
}

export class FakeSimClient implements SimClient {
  statusResult: SimResult<StatusResponse> = { ok: true, data: sampleStatusResponse() };
  stimulateResult: SimResult<{ eventId: number }> = { ok: true, data: { eventId: 1 } };
  chatResult: SimResult<{ eventId: number }> = { ok: true, data: { eventId: 1 } };
  healthzResult: SimResult<HealthzResponse> = { ok: true, data: { status: 'ok' } };
  stimulateCalls: StimulateRequest[] = [];
  /** Every `POST /chat` payload this fake was handed, in order. */
  chatCalls: ChatRequest[] = [];

  async status(): Promise<SimResult<StatusResponse>> {
    return this.statusResult;
  }

  async stimulate(request: StimulateRequest): Promise<SimResult<{ eventId: number }>> {
    this.stimulateCalls.push(request);
    return this.stimulateResult;
  }

  async chat(request: ChatRequest): Promise<SimResult<{ eventId: number }>> {
    this.chatCalls.push(request);
    return this.chatResult;
  }

  async events(): Promise<SimResult<EventsResponse>> {
    return { ok: true, data: { events: [] } };
  }

  async healthz(): Promise<SimResult<HealthzResponse>> {
    return this.healthzResult;
  }
}
