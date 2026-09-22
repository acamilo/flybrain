/**
 * `@flybrain/session-types`: the session framework contracts in TypeScript.
 *
 * The other half of `services/flysim/crates/fly-session-types`. Same rules, same canonical
 * JSON, same digests, same fixtures. Nothing here opens a socket: it reads, validates and
 * hashes payloads.
 */
export * from './canonical';
export * from './scalar';
export * from './reader';
export * from './common';
export * from './media';
export * from './workers';
export * from './rpc';
export * from './publishing';
export * from './trace';
export * as seed from './seed';
export * as checkpoint from './checkpoint';
export * as fixtures from './fixtures';
