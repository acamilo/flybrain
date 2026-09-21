/**
 * Deterministic 32-bit xorshift generator.
 *
 * Bit-exact with the original inline kernel RNG (`value ^= value << 13; value ^= value >>> 17;
 * value ^= value << 5`), including the signed internal state that checkpoints store verbatim and
 * the `/ 2**32` mapping to [0, 1). The state is kept raw (never coerced on assignment) so that
 * importing a checkpoint reproduces the original stream exactly; the shift operators perform the
 * int32 coercion, as they did in the original code.
 */
export class Xorshift32 {
  private value: number;

  constructor(seed = 22_222) {
    this.value = seed;
  }

  /** Next raw draw as an unsigned 32-bit integer. */
  nextUint(): number {
    let value = this.value;
    value ^= value << 13;
    value ^= value >>> 17;
    value ^= value << 5;
    this.value = value;
    return value >>> 0;
  }

  /** Next draw mapped to [0, 1) with 2^-32 resolution. */
  next(): number {
    return this.nextUint() / 0x1_0000_0000;
  }

  /** Signed internal state, as written to and read from checkpoints. */
  get state(): number {
    return this.value;
  }

  set state(value: number) {
    this.value = value;
  }
}
