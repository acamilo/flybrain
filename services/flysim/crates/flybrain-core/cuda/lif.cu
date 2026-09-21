// Per-millisecond LIF tick on CUDA, bit-exact with the CPU kernel `lif-1ms-f64-v2`.
//
// Mirrors steps 1-4 and 6 of `LifNetwork::step_one` in `src/lif.rs`: the noise kicks, the visual
// drive, the stimulation drive, the integrate-and-fire sweep, and spike propagation. Plasticity
// `observe`, the role tally and the rate EMAs stay on the host.
//
// Bit-exactness rules, all of which the host side re-checks against the CPU kernel:
//
//   * every intermediate is `double`; every store to a membrane rounds once with `(float)`, which
//     is IEEE round-to-nearest-even and therefore `Math.fround` / Rust's `as f32`;
//   * no FMA contraction (compiled with `--fmad=false`), no fast math, no reassociation;
//   * the only transcendental, `exp(-1/decayMs)`, is evaluated on the host at config time and
//     arrives as the already-frounded `decay` argument;
//   * propagation is a thread-per-target pull over a transposed CSR whose incoming edges are
//     ordered by base edge index, which is the source-major order the CPU push visits them in.
//     Skipping non-spiking sources leaves a subsequence in that same order, so each target sees
//     exactly the CPU's sequence of additions, with the f32 rounding and the per-edge floor clamp
//     at the same points.

#define LIF_BLOCK 256u

extern "C" {

// `Math.max`, which propagates NaN (unlike fmax) and returns +0 for max(-0, +0).
// Mirrors `jsmath::js_max`.
__device__ __forceinline__ double js_max(double a, double b) {
  if (!(a == a) || !(b == b)) {
    return __longlong_as_double(0x7ff8000000000000LL);
  }
  if (a > b) {
    return a;
  }
  if (b > a) {
    return b;
  }
  if (a == 0.0 && b == 0.0) {
    return (__double_as_longlong(a) >= 0LL) ? a : b;
  }
  return a;
}

// `lif::floor_clamp`: the common case first, `js_max` for the rest.
__device__ __forceinline__ double floor_clamp(double value, double floor) {
  return (value > floor) ? value : js_max(floor, value);
}

// Step 1. One thread per *distinct* neuron drawn this tick, applying that neuron's kicks in draw
// order. The host groups the 300 draws by neuron, so repeats (which the CPU applies in sequence
// through one f32 store each) stay sequential here too, and distinct neurons cannot interact.
__global__ void lif_noise(float *membrane, const unsigned int *neuron, const unsigned int *count,
                          unsigned int groups, double amount) {
  unsigned int group = blockIdx.x * blockDim.x + threadIdx.x;
  if (group >= groups) {
    return;
  }
  unsigned int index = neuron[group];
  unsigned int kicks = count[group];
  double value = (double)membrane[index];
  for (unsigned int kick = 0; kick < kicks; ++kick) {
    value = (double)(float)(value + amount);
  }
  membrane[index] = (float)value;
}

// Step 2. One thread per distinct visual target; `ptr`/`pos` list that neuron's columns in
// ascending column order, so a neuron that appears twice in `visual_indices` is still summed in
// the CPU's order.
__global__ void lif_visual(float *membrane, const unsigned int *neuron, const unsigned int *ptr,
                           const unsigned int *pos, const float *drive, unsigned int groups) {
  unsigned int group = blockIdx.x * blockDim.x + threadIdx.x;
  if (group >= groups) {
    return;
  }
  unsigned int index = neuron[group];
  double value = (double)membrane[index];
  for (unsigned int slot = ptr[group]; slot < ptr[group + 1]; ++slot) {
    value = (double)(float)(value + (double)drive[pos[slot]]);
  }
  membrane[index] = (float)value;
}

// Step 3. Same shape as the noise kicks: the drive is one constant, so a repeated target only
// needs its count.
__global__ void lif_stimulate(float *membrane, const unsigned int *neuron,
                              const unsigned int *count, unsigned int groups, double drive) {
  unsigned int group = blockIdx.x * blockDim.x + threadIdx.x;
  if (group >= groups) {
    return;
  }
  unsigned int index = neuron[group];
  unsigned int hits = count[group];
  double value = (double)membrane[index];
  for (unsigned int hit = 0; hit < hits; ++hit) {
    value = (double)(float)(value + drive);
  }
  membrane[index] = (float)value;
}

// Step 4. Elementwise integrate-and-fire, one thread per neuron, in the same `double` expression
// `lif::sweep_one` uses. Spikes leave a per-warp ballot word in `spiked_bits` (which propagation
// reads directly) plus a per-block count for the compaction scan; the ascending spike *list* is
// built by `lif_compact` from those bits.
__global__ void lif_sweep(float *membrane, unsigned char *refractory, const float *baseline,
                          unsigned int neurons, double decay, double threshold,
                          unsigned int refractory_reset, unsigned int *spiked_bits,
                          unsigned int *block_counts) {
  unsigned int index = blockIdx.x * blockDim.x + threadIdx.x;
  bool spiked = false;
  if (index < neurons) {
    unsigned char left = refractory[index];
    if (left > 0) {
      // A refractory neuron still leaks but receives no baseline drive.
      refractory[index] = (unsigned char)(left - 1);
      membrane[index] = (float)((double)membrane[index] * decay);
    } else {
      double voltage = (double)membrane[index] * decay + (double)baseline[index];
      if (voltage >= threshold) {
        // A spiking neuron resets to exactly 0 rather than subtracting the threshold.
        membrane[index] = 0.0f;
        refractory[index] = (unsigned char)refractory_reset;
        spiked = true;
      } else {
        membrane[index] = (float)voltage;
      }
    }
  }

  unsigned int mask = __ballot_sync(0xffffffffu, spiked);
  unsigned int lane = threadIdx.x & 31u;
  unsigned int warp = threadIdx.x >> 5;
  __shared__ unsigned int per_warp[LIF_BLOCK / 32u];
  if (lane == 0u) {
    unsigned int word = (blockIdx.x * blockDim.x + warp * 32u) >> 5;
    if (word < ((neurons + 31u) >> 5)) {
      spiked_bits[word] = mask;
    }
    per_warp[warp] = (unsigned int)__popc(mask);
  }
  __syncthreads();
  if (threadIdx.x == 0u) {
    unsigned int total = 0u;
    for (unsigned int slot = 0u; slot < (blockDim.x >> 5); ++slot) {
      total += per_warp[slot];
    }
    block_counts[blockIdx.x] = total;
  }
}

// Exclusive scan of the per-block spike counts, in one block, so `lif_compact` can write straight
// into the batch's flat spike buffer. Also records this tick's spike count and advances the shared
// write cursor, which keeps the whole batch's spike lists packed for a single download.
__global__ void lif_scan(const unsigned int *block_counts, unsigned int *block_offsets,
                         unsigned int blocks, unsigned int *tick_counts, unsigned int *tick_base,
                         unsigned int tick, unsigned int *cursor) {
  extern __shared__ unsigned int partial[];
  unsigned int thread = threadIdx.x;
  unsigned int threads = blockDim.x;
  unsigned int chunk = (blocks + threads - 1u) / threads;
  unsigned int from = thread * chunk;
  unsigned int to = from + chunk;
  if (from > blocks) {
    from = blocks;
  }
  if (to > blocks) {
    to = blocks;
  }

  unsigned int sum = 0u;
  for (unsigned int block = from; block < to; ++block) {
    sum += block_counts[block];
  }
  partial[thread] = sum;
  __syncthreads();
  for (unsigned int offset = 1u; offset < threads; offset <<= 1) {
    unsigned int carry = (thread >= offset) ? partial[thread - offset] : 0u;
    __syncthreads();
    partial[thread] += carry;
    __syncthreads();
  }

  unsigned int base = *cursor;
  unsigned int running = base + (partial[thread] - sum);
  for (unsigned int block = from; block < to; ++block) {
    block_offsets[block] = running;
    running += block_counts[block];
  }
  __syncthreads();
  if (thread == threads - 1u) {
    tick_counts[tick] = partial[threads - 1u];
    tick_base[tick] = base;
    *cursor = base + partial[threads - 1u];
  }
}

// The ascending spike list. Block offsets ascend with the neuron index and the rank inside a block
// is a ballot prefix, so the list is exactly the sequential kernel's: `observe` and the role tally
// consume it in this order.
__global__ void lif_compact(const unsigned int *spiked_bits, const unsigned int *block_offsets,
                            unsigned int neurons, unsigned int *spikes) {
  unsigned int index = blockIdx.x * blockDim.x + threadIdx.x;
  unsigned int lane = threadIdx.x & 31u;
  unsigned int warp = threadIdx.x >> 5;
  unsigned int word = (blockIdx.x * blockDim.x + warp * 32u) >> 5;
  unsigned int mask = (word < ((neurons + 31u) >> 5)) ? spiked_bits[word] : 0u;

  __shared__ unsigned int per_warp[LIF_BLOCK / 32u];
  if (lane == 0u) {
    per_warp[warp] = (unsigned int)__popc(mask);
  }
  __syncthreads();

  if (index < neurons && ((mask >> lane) & 1u) != 0u) {
    unsigned int rank = (unsigned int)__popc(mask & ((1u << lane) - 1u));
    for (unsigned int slot = 0u; slot < warp; ++slot) {
      rank += per_warp[slot];
    }
    spikes[block_offsets[blockIdx.x] + rank] = index;
  }
}

// Step 6, part 1 of 4: how many of this tick's edges land on each target.
//
// The pull formulation this replaced — a warp per target over a transposed CSR, skipping sources
// that did not spike — is bit-exact and was measured at 320 us per tick, because it has to read
// all 2,700,513 incoming edges every tick while the CPU push reads only the ~1.2 % that leave a
// spiking neuron (about 33,000 edges). That asymmetry, not the arithmetic, is what made the GPU
// slower than four CPU cores. So: push the spiking rows into per-target buckets, then let one
// thread per target sum its own bucket in the CPU's order.
//
// Bucket *placement* uses atomics and is therefore in an arbitrary order; `lif_bucket_apply`
// restores the order by sorting each bucket on the base edge index, which is exactly the CPU's
// order (the sequential kernel walks spiking sources ascending and each row ascending, and the
// base edge index is source-major then slot). Buckets hold a handful of entries, so the sort is
// free and determinism costs nothing.
//
// A grid-stride loop over the spiking sources, one warp each, so the row reads are coalesced and
// the grid does not have to be sized for the worst case.
__global__ void lif_bucket_count(const unsigned int *__restrict__ spikes,
                                 const unsigned int *__restrict__ tick_counts,
                                 const unsigned int *__restrict__ tick_base, unsigned int tick,
                                 const unsigned int *__restrict__ indptr,
                                 const unsigned int *__restrict__ targets, unsigned int *count) {
  unsigned int spiking = tick_counts[tick];
  unsigned int base = tick_base[tick];
  unsigned int warps = (gridDim.x * blockDim.x) >> 5;
  unsigned int warp = (blockIdx.x * blockDim.x + threadIdx.x) >> 5;
  unsigned int lane = threadIdx.x & 31u;
  for (unsigned int index = warp; index < spiking; index += warps) {
    unsigned int source = spikes[base + index];
    unsigned int from = indptr[source];
    unsigned int to = indptr[source + 1u];
    for (unsigned int edge = from + lane; edge < to; edge += 32u) {
      atomicAdd(&count[targets[edge]], 1u);
    }
  }
}

// Step 6, part 2 of 4: the same walk again, writing each edge into its target's bucket.
//
// A bucket entry is one `unsigned long long`: the base edge index in the top 32 bits, the edge's
// `short` weight in the bottom 16, and the plastic gain slot plus one (0 for the immutable
// majority) in bits 16-30. The edge index being the most significant field means sorting the
// entries as integers sorts them by edge index, which is the CPU's order.
//
// Resolving the weight and the gain slot *here* rather than in `lif_bucket_apply` is worth 80 us
// per tick: here the reads follow a CSR row, so a warp's 32 lanes read consecutive weights and
// share a plastic-bitset word; there they would be 97,000 scattered reads by whichever thread owns
// the target. The arithmetic is unaffected — the same `short` and the same `f32` gain reach the
// same f64 expression.
__global__ void lif_bucket_place(const unsigned int *__restrict__ spikes,
                                 const unsigned int *__restrict__ tick_counts,
                                 const unsigned int *__restrict__ tick_base, unsigned int tick,
                                 const unsigned int *__restrict__ indptr,
                                 const unsigned int *__restrict__ targets,
                                 const short *__restrict__ weights,
                                 const unsigned long long *__restrict__ plastic_words,
                                 const unsigned int *__restrict__ plastic_prefix,
                                 unsigned int *cursor, unsigned long long *bucket) {
  unsigned int spiking = tick_counts[tick];
  unsigned int base = tick_base[tick];
  unsigned int warps = (gridDim.x * blockDim.x) >> 5;
  unsigned int warp = (blockIdx.x * blockDim.x + threadIdx.x) >> 5;
  unsigned int lane = threadIdx.x & 31u;
  for (unsigned int index = warp; index < spiking; index += warps) {
    unsigned int source = spikes[base + index];
    unsigned int from = indptr[source];
    unsigned int to = indptr[source + 1u];
    for (unsigned int edge = from + lane; edge < to; edge += 32u) {
      unsigned long long word = plastic_words[edge >> 6];
      unsigned int slot = 0u;
      if (((word >> (edge & 63u)) & 1ULL) != 0ULL) {
        unsigned long long below = word & ((1ULL << (edge & 63u)) - 1ULL);
        slot = plastic_prefix[edge >> 6] + (unsigned int)__popcll(below) + 1u;
      }
      unsigned long long entry = ((unsigned long long)edge << 32) |
                                 (unsigned long long)((unsigned int)(unsigned short)weights[edge]) |
                                 ((unsigned long long)slot << 16);
      bucket[atomicAdd(&cursor[targets[edge]], 1u)] = entry;
    }
  }
}

// Step 6, part 3 of 4: an exclusive scan of the bucket sizes, in two kernels plus the small
// single-block scan below. `offset` holds the block-local exclusive scan on the way in and the
// global one on the way out.
__global__ void lif_bucket_scan(const unsigned int *__restrict__ count, unsigned int *offset,
                                unsigned int *block_sums, unsigned int neurons) {
  __shared__ unsigned int partial[LIF_BLOCK];
  unsigned int index = blockIdx.x * blockDim.x + threadIdx.x;
  unsigned int thread = threadIdx.x;
  unsigned int mine = (index < neurons) ? count[index] : 0u;
  partial[thread] = mine;
  __syncthreads();
  for (unsigned int step = 1u; step < blockDim.x; step <<= 1) {
    unsigned int carry = (thread >= step) ? partial[thread - step] : 0u;
    __syncthreads();
    partial[thread] += carry;
    __syncthreads();
  }
  if (index < neurons) {
    offset[index] = partial[thread] - mine;
  }
  if (thread == blockDim.x - 1u) {
    block_sums[blockIdx.x] = partial[thread];
  }
}

// Exclusive scan of up to `blockDim.x` * chunk values, in one block. Used for the bucket scan's
// block sums.
__global__ void lif_exscan(const unsigned int *__restrict__ input, unsigned int *output,
                           unsigned int count) {
  extern __shared__ unsigned int partial[];
  unsigned int thread = threadIdx.x;
  unsigned int threads = blockDim.x;
  unsigned int chunk = (count + threads - 1u) / threads;
  unsigned int from = thread * chunk;
  unsigned int to = from + chunk;
  if (from > count) {
    from = count;
  }
  if (to > count) {
    to = count;
  }
  unsigned int sum = 0u;
  for (unsigned int index = from; index < to; ++index) {
    sum += input[index];
  }
  partial[thread] = sum;
  __syncthreads();
  for (unsigned int step = 1u; step < threads; step <<= 1) {
    unsigned int carry = (thread >= step) ? partial[thread - step] : 0u;
    __syncthreads();
    partial[thread] += carry;
    __syncthreads();
  }
  unsigned int running = partial[thread] - sum;
  for (unsigned int index = from; index < to; ++index) {
    output[index] = running;
    running += input[index];
  }
}

// Add each block's base to its block-local scan, and seed the placement cursor.
__global__ void lif_bucket_rebase(unsigned int *offset, unsigned int *cursor,
                                  const unsigned int *__restrict__ block_bases,
                                  unsigned int neurons) {
  unsigned int index = blockIdx.x * blockDim.x + threadIdx.x;
  if (index >= neurons) {
    return;
  }
  unsigned int value = offset[index] + block_bases[blockIdx.x];
  offset[index] = value;
  cursor[index] = value;
}

// Step 6, part 4 of 4: one thread per target sums its own bucket, in the CPU's order.
//
// Every target's membrane depends on nothing but its own previous value and its own sequence of
// addends, so this is where bit-exactness is won or lost: the bucket is sorted on the base edge
// index, each addend is the same `weight * gain * synapse_scale` product evaluated in the same
// order, and the f32 rounding and the per-edge floor clamp happen at the same points.
__global__ void lif_bucket_apply(float *membrane, const unsigned int *__restrict__ count,
                                 const unsigned int *__restrict__ offset,
                                 unsigned long long *bucket, const float *__restrict__ gains,
                                 unsigned int neurons, double synapse_scale,
                                 double membrane_floor) {
  unsigned int target = blockIdx.x * blockDim.x + threadIdx.x;
  if (target >= neurons) {
    return;
  }
  unsigned int entries = count[target];
  if (entries == 0u) {
    return;
  }
  unsigned int from = offset[target];

  // Sort the bucket, whose placement order is whatever the atomics produced.
  //
  // Shell sort with Ciura's gaps, not plain insertion sort. Buckets average well under one entry,
  // so the gap loop degenerates to insertion sort for almost every target — but the neuron with
  // the maximum in-degree (5,080 on fafb-v783) collects a couple of hundred entries in a tick, one
  // thread owns all of them, and that thread's k^2 was measured as 100 us of the tick. Any correct
  // sort preserves bit-exactness, because the keys are distinct edge indices.
  const unsigned int gaps[8] = {701u, 301u, 132u, 57u, 23u, 10u, 4u, 1u};
  for (unsigned int step = 0u; step < 8u; ++step) {
    unsigned int gap = gaps[step];
    if (gap >= entries) {
      continue;
    }
    for (unsigned int index = gap; index < entries; ++index) {
      unsigned long long key = bucket[from + index];
      unsigned int at = index;
      while (at >= gap && bucket[from + at - gap] > key) {
        bucket[from + at] = bucket[from + at - gap];
        at -= gap;
      }
      bucket[from + at] = key;
    }
  }

  double value = (double)membrane[target];
  for (unsigned int index = 0u; index < entries; ++index) {
    unsigned long long entry = bucket[from + index];
    short weight = (short)(unsigned short)(entry & 0xffffULL);
    unsigned int slot = (unsigned int)((entry >> 16) & 0x7fffULL);
    double gain = (slot == 0u) ? 1.0 : (double)gains[slot - 1u];
    // The floor is applied per edge, not once per tick: the clamp order is part of the numerics.
    double next = value + (double)weight * gain * synapse_scale;
    value = (double)(float)floor_clamp(next, membrane_floor);
  }
  membrane[target] = (float)value;
}

}  // extern "C"
