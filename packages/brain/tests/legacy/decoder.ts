import { BUTTONS, BUTTON_BITS, type Button } from './protocol';

const RATE_GROUP: Record<Button, string> = {
  up: 'command_0', down: 'command_1', left: 'command_2', right: 'command_3',
  a: 'command_4', b: 'command_5', start: 'command_6', select: 'command_7',
};

/** Fixed readout: plasticity belongs to the neural network, not button gains. */
export class MotorDecoder {
  private readonly baseline: Record<string, number> = {};
  private readonly heldUntil = Object.fromEntries(BUTTONS.map(button => [button, 0])) as Record<Button, number>;
  private readonly nextAllowed = Object.fromEntries(BUTTONS.map(button => [button, 0])) as Record<Button, number>;
  private calibrated = false;
  private nextDirectionDecision = 0;
  private direction: Button | null = null;
  private fatigue: Record<string, number> = { up: 0, down: 0, left: 0, right: 0 };

  clearHolds(nowMs: number): void {
    for (const button of BUTTONS) { this.heldUntil[button] = 0; this.nextAllowed[button] = nowMs + 480; }
    this.direction = null;
    for (const key of Object.keys(this.fatigue)) this.fatigue[key] = 0;
    this.nextDirectionDecision = nowMs;
  }

  calibrate(rates: Record<string, number>): void {
    for (const name of Object.values(RATE_GROUP)) this.baseline[name] = rates[name] ?? 0;
    this.calibrated = true;
  }

  decode(rates: Record<string, number>, nowMs: number, boot = true): number {
    if (!this.calibrated) return 0;
    const scores = Object.fromEntries(BUTTONS.map(button => {
      const name = RATE_GROUP[button];
      return [button, ((rates[name] ?? 0) + 1) / ((this.baseline[name] ?? 0) + 1)];
    })) as Record<Button, number>;
    const directions = ['up', 'down', 'left', 'right'] as const;
    if (nowMs >= this.nextDirectionDecision) {
      // Bounded motor habituation prevents a small persistent rate bias from
      // holding one command forever. No game coordinates enter this readout.
      for (const direction of directions) scores[direction] /= 1 + this.fatigue[direction];
      let best = directions.reduce((a, b) => scores[b] > scores[a] ? b : a);
      if (this.direction && scores[best] < scores[this.direction] * 1.15) best = this.direction as typeof best;
      for (const direction of directions) this.heldUntil[direction] = 0;
      this.direction = best;
      for (const direction of directions) this.fatigue[direction] = direction === best ? Math.min(1, this.fatigue[direction] + 0.08) : this.fatigue[direction] * 0.8;
      this.heldUntil[best] = nowMs + 400;
      this.nextDirectionDecision = nowMs + 400;
    }
    for (const button of ['a', 'b', 'start', 'select'] as const) {
      const system = button === 'start' || button === 'select';
      if (scores[button] > (system && !boot ? 1.35 : 1) && nowMs >= this.nextAllowed[button]) {
        this.heldUntil[button] = nowMs + (system ? 55 : 85);
        this.nextAllowed[button] = nowMs + (system ? (boot ? 2500 : 30000) : 480);
        if (system) this.nextAllowed[button === 'start' ? 'select' : 'start'] = this.nextAllowed[button];
      }
    }
    let mask = 0;
    for (const button of BUTTONS) if (nowMs < this.heldUntil[button]) mask |= BUTTON_BITS[button];
    return mask;
  }

  exportState() {
    return { version: 3, fatigue: { ...this.fatigue }, direction: this.direction, baseline: { ...this.baseline }, heldUntil: { ...this.heldUntil }, nextAllowed: { ...this.nextAllowed }, calibrated: this.calibrated, nextDirectionDecision: this.nextDirectionDecision };
  }

  importState(state: ReturnType<MotorDecoder['exportState']>): void {
    if (!state || typeof state.calibrated !== 'boolean' || !Number.isFinite(state.nextDirectionDecision) || [state.baseline, state.heldUntil, state.nextAllowed].some(record => !record || Object.values(record).some(value => !Number.isFinite(value))) || BUTTONS.some(button => !Number.isFinite(state.heldUntil[button]) || !Number.isFinite(state.nextAllowed[button]))) throw new Error('Invalid decoder checkpoint');
    Object.assign(this.baseline, state.baseline);
    Object.assign(this.heldUntil, state.heldUntil);
    Object.assign(this.nextAllowed, state.nextAllowed);
    this.calibrated = state.calibrated;
    this.nextDirectionDecision = state.nextDirectionDecision;
    if (state.version !== undefined && state.version !== 2 && state.version !== 3) throw new Error('Invalid decoder version');
    if (state.version === 3) {
      if (!state.fatigue || Object.keys(this.fatigue).some(key => !Number.isFinite(state.fatigue[key]) || state.fatigue[key] < 0 || state.fatigue[key] > 1)) throw new Error('Invalid motor habituation');
      this.fatigue = { ...state.fatigue };
    }
    if (state.direction != null && !['up', 'down', 'left', 'right'].includes(state.direction)) throw new Error('Invalid decoder direction');
    this.direction = state.direction ?? null;
  }
}
