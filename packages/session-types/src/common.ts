/**
 * `Scope`, `SchemaRef`, `TypedValue`, the operation key and the canonical body
 * (ipc-v1 sections 2 and 5).
 */
import { canonicalize, digestOf, fail, rejectBusIdentities } from './canonical';
import { Reader } from './reader';
import {
  MAX_TYPED_VALUE_BYTES,
  type RationalNs,
  type Scope,
  type SchemaRef,
  type TypedValue,
  isId,
  isMethod,
  validateRational,
} from './scalar';

export function readScope(value: unknown): Scope {
  const reader = new Reader(value, 'Scope');
  const scope: Scope = {
    sessionId: reader.id('sessionId'),
    epoch: reader.id('epoch'),
    step: reader.u64('step'),
  };
  reader.finish();
  return scope;
}

export function readNullableScope(value: unknown): Scope | null {
  return value === null ? null : readScope(value);
}

export function readSchemaRef(value: unknown): SchemaRef {
  const reader = new Reader(value, 'SchemaRef');
  const schema: SchemaRef = {
    id: reader.id('id'),
    version: reader.int('version', 1, 65_535),
    digest: reader.digest('digest'),
  };
  reader.finish();
  return schema;
}

export function readTypedValue(value: unknown): TypedValue {
  const reader = new Reader(value, 'TypedValue');
  const typed: TypedValue = {
    schema: readSchemaRef(reader.value('schema')),
    value: reader.object('value'),
  };
  reader.finish();
  const length = canonicalize(typed).length;
  if (length > MAX_TYPED_VALUE_BYTES) {
    fail(
      `TypedValue: ${length} bytes of canonical JSON exceeds the ${MAX_TYPED_VALUE_BYTES}-byte limit`,
    );
  }
  return typed;
}

export function readNullableTypedValue(value: unknown): TypedValue | null {
  return value === null ? null : readTypedValue(value);
}

export { validateRational };

/** `(sessionId, epoch, step, method, workerId)`: the operation key of a step mutation. */
export interface OperationKey {
  scope: Scope;
  method: string;
  workerId: string;
}

export function operationKeyJson(key: OperationKey): Record<string, unknown> {
  if (!isMethod(key.method)) {
    fail('OperationKey: method must be 1..=128 printable ASCII characters');
  }
  if (!isId(key.workerId)) fail('OperationKey: workerId is not a valid id');
  return { scope: key.scope, method: key.method, workerId: key.workerId };
}

export function operationKeyDigest(key: OperationKey): string {
  return digestOf(operationKeyJson(key));
}

/** The canonical body of a domain operation: method, scope and validated params. */
export function canonicalBody(
  method: string,
  scope: Scope | null,
  params: unknown,
): Record<string, unknown> {
  if (!isMethod(method)) {
    fail('canonical body: method must be 1..=128 printable ASCII characters');
  }
  if (params === null || typeof params !== 'object' || Array.isArray(params)) {
    fail('canonical body: params must be an object');
  }
  rejectBusIdentities(params);
  return { method, scope, params };
}

export function bodyDigest(method: string, scope: Scope | null, params: unknown): string {
  return digestOf(canonicalBody(method, scope, params));
}

export function readRational(value: unknown): RationalNs {
  const reader = new Reader(value, 'RationalNs');
  const rational: RationalNs = {
    numerator: reader.u64('numerator'),
    denominator: reader.u64('denominator'),
  };
  reader.finish();
  validateRational(rational);
  return rational;
}
