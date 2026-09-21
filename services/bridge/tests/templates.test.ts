import assert from 'node:assert/strict';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { renderTemplate, TEMPLATE_IDS, EXPLAINER_TEMPLATE_IDS, isTemplateId } from '../src/templates';

const SRC_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', 'src');

void test('every TEMPLATE_IDS entry renders without throwing, given its params', () => {
  const gameTitle = 'Pokemon Red';
  const sampleParams: Record<string, unknown> = {
    startup: { channel: 'flyplayspokemon', gameTitle },
    recovered: { channel: 'flyplayspokemon', gameTitle },
    fly: { gameTitle },
    brain: { gameTitle },
    how: { gameTitle },
    stuck: { label: 'Left the bedroom', durationLabel: '3m 12s' },
    sugarAccepted: { by: 'fly_fan_42' },
    sugarCooldown: { retryAfterSeconds: 7 },
    sugarDisabled: {},
    followThanks: { by: 'fly_fan_42' },
    raidThanks: { by: 'fly_fan_42', viewers: 12 },
    explainerConnectome: {},
    explainerButtons: { gameTitle },
    explainerReward: { gameTitle },
    explainerSugar: {},
    explainerHonesty: {},
    explainerRepo: {},
  };

  for (const id of TEMPLATE_IDS) {
    const params = sampleParams[id];
    assert.ok(params !== undefined, `no sample params registered for template ${id}`);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const text = renderTemplate(id, params as any);
    assert.equal(typeof text, 'string');
    assert.ok(text.length > 0, `template ${id} rendered an empty string`);
  }
});

void test('EXPLAINER_TEMPLATE_IDS has exactly six cards, all valid template ids', () => {
  assert.equal(EXPLAINER_TEMPLATE_IDS.length, 6);
  for (const id of EXPLAINER_TEMPLATE_IDS) assert.ok(isTemplateId(id));
});

void test('templates interpolate hostile-looking but validated display names verbatim', () => {
  // These are display names that already passed src/names.ts's validateDisplayName (or the
  // fallback "a viewer") — templates trust the caller and just interpolate. This test documents
  // that the interpolation itself does not choke on punctuation-free unicode names, and that
  // nothing here does HTML/markup escaping that would mangle a legitimate name.
  const hostileButValid = ['a_viewer', '雨宮', 'Ω_2', 'a'.repeat(25)];
  for (const by of hostileButValid) {
    assert.ok(renderTemplate('sugarAccepted', { by }).includes(by));
    assert.ok(renderTemplate('followThanks', { by }).includes(by));
  }
  // The literal fallback text itself must also render fine.
  assert.ok(renderTemplate('sugarAccepted', { by: 'a viewer' }).includes('a viewer'));
});

void test('renderTemplate never returns a string containing "undefined" for numeric params', () => {
  assert.ok(!renderTemplate('sugarCooldown', { retryAfterSeconds: 0 }).includes('undefined'));
  assert.ok(!renderTemplate('raidThanks', { by: 'x', viewers: 0 }).includes('undefined'));
});

// -- Lint: every send(...) call site in src/ must pass a TEMPLATE_IDS string literal -----------

interface SendCallSite {
  file: string;
  line: number;
  firstArgText: string;
}

function collectSourceFiles(dir: string): string[] {
  const entries = readdirSync(dir);
  const files: string[] = [];
  for (const entry of entries) {
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      files.push(...collectSourceFiles(full));
    } else if (
      entry.endsWith('.ts') &&
      entry !== 'templates.ts' &&
      entry !== 'chat.ts' &&
      entry !== 'onscreen-chat.ts'
    ) {
      // templates.ts defines renderTemplate/send itself; chat.ts defines createSend/Send and is
      // exempted from the call-site check for the same reason (it has no `send(...)` call sites
      // of its own — it only *defines* send). onscreen-chat.ts is exempt on the same grounds: its
      // only `send(...)` is inside `wrapSendWithOnscreenEcho`, a combinator that forwards whatever
      // `TemplateId` its caller already passed, which the type system checks and this lint checks
      // at that caller's own call site.
      files.push(full);
    }
  }
  return files;
}

/**
 * Find every `send(<firstArg>` call, capturing the raw text of the first argument.
 *
 * Comments are blanked first — prose that mentions `send()` is documentation about this rule, not
 * a call site that breaks it — while keeping line numbers intact so a violation still points at
 * the right line.
 */
function findSendCalls(text: string): { firstArgText: string; line: number }[] {
  const source = text
    .replace(/\/\*[\s\S]*?\*\//g, (block) => block.replace(/[^\n]/g, ' '))
    .replace(/(^|[^:])\/\/[^\n]*/g, (line) => line.replace(/[^\n]/g, ' '));
  const results: { firstArgText: string; line: number }[] = [];
  const regex = /\bsend\(\s*([^,)]*)/g;
  let match: RegExpExecArray | null;
  while ((match = regex.exec(source)) !== null) {
    const firstArgText = match[1]!.trim();
    const upToMatch = source.slice(0, match.index);
    const line = upToMatch.split('\n').length;
    results.push({ firstArgText, line });
  }
  return results;
}

void test('every send(...) call site in src/ passes a string literal from TEMPLATE_IDS', () => {
  const files = collectSourceFiles(SRC_DIR);
  const violations: SendCallSite[] = [];
  let sawAnyCallSite = false;

  const stringLiteral = /^(['"])((?:[^\\]|\\.)*?)\1$/;

  for (const file of files) {
    const source = readFileSync(file, 'utf8');
    for (const call of findSendCalls(source)) {
      sawAnyCallSite = true;
      const literalMatch = stringLiteral.exec(call.firstArgText);
      if (!literalMatch || !isTemplateId(literalMatch[2]!)) {
        violations.push({ file, line: call.line, firstArgText: call.firstArgText });
      }
    }
  }

  assert.ok(sawAnyCallSite, 'expected at least one send(...) call site in src/ to check');
  assert.deepEqual(
    violations,
    [],
    `send() called with a non-constant or unknown TemplateId:\n${violations
      .map((v) => `  ${v.file}:${v.line}: send(${v.firstArgText}, ...)`)
      .join('\n')}`,
  );
});
