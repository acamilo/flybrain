import assert from 'node:assert/strict';
import test from 'node:test';
import { ConfigError, loadConfig, type Env } from '../src/config';

function baseEnv(overrides: Env = {}): Env {
  return {
    TWITCH_CLIENT_ID: 'client-id',
    TWITCH_CLIENT_SECRET: 'client-secret',
    CHANNEL: 'flyplayspokemon',
    BOT_USER: 'flybridgebot',
    GAME_TITLE: 'Pokemon Red',
    ...overrides,
  };
}

void test('loadConfig fills defaults for a minimal valid env', () => {
  const config = loadConfig({ env: baseEnv() });
  assert.equal(config.channel, 'flyplayspokemon');
  assert.equal(config.botUser, 'flybridgebot');
  assert.equal(config.gameTitle, 'Pokemon Red');
  assert.equal(config.simControlUrl, 'http://127.0.0.1:7401');
  assert.equal(config.tokensFile, '/var/lib/flybridge/tokens.json');
  assert.equal(config.healthHost, '127.0.0.1');
  assert.equal(config.healthPort, 7410);
  assert.equal(config.featureRedemptions, false);
  assert.equal(config.featurePredictions, false);
  // On-screen chat is the one feature flag that defaults ON: the CHAT panel is part of the locked
  // rail layout, and flysim's `[chat] enabled` is the switch for turning it off in a hurry.
  assert.equal(config.featureOnscreenChat, true);
  assert.equal(config.rateLimits.perUserPerCommandMs, 60_000);
  assert.equal(config.rateLimits.globalReplyMs, 5_000);
});

void test('loadConfig reads overrides from env', () => {
  const config = loadConfig({
    env: baseEnv({
      SIM_CONTROL_URL: 'http://127.0.0.1:9999',
      TOKENS_FILE: '/tmp/tokens.json',
      HEALTH_PORT: '9410',
      FEATURE_REDEMPTIONS: 'true',
      FEATURE_PREDICTIONS: '1',
      FEATURE_ONSCREEN_CHAT: 'off',
    }),
  });
  assert.equal(config.simControlUrl, 'http://127.0.0.1:9999');
  assert.equal(config.tokensFile, '/tmp/tokens.json');
  assert.equal(config.healthPort, 9410);
  assert.equal(config.featureRedemptions, true);
  assert.equal(config.featurePredictions, true);
  assert.equal(config.featureOnscreenChat, false);
});

void test('loadConfig parses the combined two-line twitch-app credential', () => {
  // The shape infra/06-secrets.sh actually installs: one file, "<id>\n<secret>\n".
  const config = loadConfig({
    env: baseEnv({ CREDENTIALS_DIRECTORY: '/run/creds', TWITCH_CLIENT_ID: undefined, TWITCH_CLIENT_SECRET: undefined }),
    readCredentialFile: (path) => {
      if (path === '/run/creds/twitch-app') return 'app-id\napp-secret\n';
      throw new Error(`ENOENT: ${path}`);
    },
  });
  assert.equal(config.twitchClientId, 'app-id');
  assert.equal(config.twitchClientSecret, 'app-secret');
});

void test('the split credential pair wins over a combined twitch-app file', () => {
  const config = loadConfig({
    env: baseEnv({ CREDENTIALS_DIRECTORY: '/run/creds', TWITCH_CLIENT_ID: undefined, TWITCH_CLIENT_SECRET: undefined }),
    readCredentialFile: (path) => {
      if (path === '/run/creds/twitch-client-id') return 'split-id\n';
      if (path === '/run/creds/twitch-client-secret') return 'split-secret\n';
      if (path === '/run/creds/twitch-app') return 'combined-id\ncombined-secret\n';
      throw new Error(`ENOENT: ${path}`);
    },
  });
  assert.equal(config.twitchClientId, 'split-id');
  assert.equal(config.twitchClientSecret, 'split-secret');
});

void test('a twitch-app credential with the wrong number of lines is refused by line count', () => {
  assert.throws(
    () =>
      loadConfig({
        env: baseEnv({ CREDENTIALS_DIRECTORY: '/run/creds', TWITCH_CLIENT_ID: undefined, TWITCH_CLIENT_SECRET: undefined }),
        readCredentialFile: (path) => {
          if (path === '/run/creds/twitch-app') return 'only-one-line\n';
          throw new Error(`ENOENT: ${path}`);
        },
      }),
    (error: unknown) => {
      assert.ok(error instanceof ConfigError);
      assert.ok(error.problems.some((problem) => problem.includes('exactly two non-empty lines')));
      return true;
    },
  );
});

void test('with no credential of either shape, the problem names both', () => {
  assert.throws(
    () =>
      loadConfig({
        env: baseEnv({ CREDENTIALS_DIRECTORY: '/run/creds', TWITCH_CLIENT_ID: undefined, TWITCH_CLIENT_SECRET: undefined }),
        readCredentialFile: (path) => {
          throw new Error(`ENOENT: ${path}`);
        },
      }),
    (error: unknown) => {
      assert.ok(error instanceof ConfigError);
      const joined = error.problems.join('\n');
      assert.ok(joined.includes('twitch-client-id'));
      assert.ok(joined.includes('twitch-app'));
      return true;
    },
  );
});

void test('loadConfig reads credentials from CREDENTIALS_DIRECTORY when set', () => {
  const files: Record<string, string> = {
    '/run/creds/twitch-client-id': 'from-file-id\n',
    '/run/creds/twitch-client-secret': 'from-file-secret\n',
  };
  const config = loadConfig({
    env: baseEnv({ CREDENTIALS_DIRECTORY: '/run/creds', TWITCH_CLIENT_ID: undefined, TWITCH_CLIENT_SECRET: undefined }),
    readCredentialFile: (path) => {
      const value = files[path];
      if (value === undefined) throw new Error(`ENOENT: ${path}`);
      return value;
    },
  });
  assert.equal(config.twitchClientId, 'from-file-id');
  assert.equal(config.twitchClientSecret, 'from-file-secret');
});

void test('loadConfig throws ConfigError collecting every missing required field', () => {
  assert.throws(
    () => loadConfig({ env: {} }),
    (error: unknown) => {
      assert.ok(error instanceof ConfigError);
      assert.ok(error.problems.some((p) => p.includes('TWITCH_CLIENT_ID')));
      assert.ok(error.problems.some((p) => p.includes('TWITCH_CLIENT_SECRET')));
      assert.ok(error.problems.some((p) => p.includes('CHANNEL')));
      assert.ok(error.problems.some((p) => p.includes('BOT_USER')));
      assert.ok(error.problems.some((p) => p.includes('GAME_TITLE')));
      assert.ok(error.problems.length >= 5, 'collects every problem, not just the first');
      return true;
    },
  );
});

void test('loadConfig rejects an invalid SIM_CONTROL_URL', () => {
  assert.throws(
    () => loadConfig({ env: baseEnv({ SIM_CONTROL_URL: 'not a url' }) }),
    (error: unknown) => {
      assert.ok(error instanceof ConfigError);
      assert.ok(error.problems.some((p) => p.includes('SIM_CONTROL_URL')));
      return true;
    },
  );
});

void test('loadConfig rejects a non-integer HEALTH_PORT', () => {
  assert.throws(
    () => loadConfig({ env: baseEnv({ HEALTH_PORT: 'not-a-number' }) }),
    (error: unknown) => {
      assert.ok(error instanceof ConfigError);
      assert.ok(error.problems.some((p) => p.includes('HEALTH_PORT')));
      return true;
    },
  );
});

void test('loadConfig rejects an unrecognized FEATURE_REDEMPTIONS value', () => {
  assert.throws(
    () => loadConfig({ env: baseEnv({ FEATURE_REDEMPTIONS: 'maybe' }) }),
    (error: unknown) => {
      assert.ok(error instanceof ConfigError);
      assert.ok(error.problems.some((p) => p.includes('FEATURE_REDEMPTIONS')));
      return true;
    },
  );
});

void test('loadConfig error message lists every problem, not just one', () => {
  try {
    loadConfig({ env: {} });
    assert.fail('expected loadConfig to throw');
  } catch (error) {
    assert.ok(error instanceof ConfigError);
    const lines = error.message.split('\n').filter((l) => l.trim().startsWith('-'));
    assert.ok(lines.length >= 5);
  }
});

// -- Quiet mode and the explainer's own off switch ----------------------------------------------

void test('FEATURE_QUIET defaults to false and parses the usual boolean spellings', () => {
  assert.equal(loadConfig({ env: baseEnv() }).featureQuiet, false);
  assert.equal(loadConfig({ env: baseEnv({ FEATURE_QUIET: '1' }) }).featureQuiet, true);
  assert.equal(loadConfig({ env: baseEnv({ FEATURE_QUIET: 'true' }) }).featureQuiet, true);
  assert.equal(loadConfig({ env: baseEnv({ FEATURE_QUIET: 'off' }) }).featureQuiet, false);
});

void test('loadConfig rejects an unrecognized FEATURE_QUIET value', () => {
  assert.throws(
    () => loadConfig({ env: baseEnv({ FEATURE_QUIET: 'quietish' }) }),
    (error: unknown) => {
      assert.ok(error instanceof ConfigError);
      assert.ok(error.problems.some((p) => p.includes('FEATURE_QUIET')));
      return true;
    },
  );
});

void test('EXPLAINER_INTERVAL_MS=0 is legal and means "no rotation", unlike every other interval', () => {
  assert.equal(loadConfig({ env: baseEnv({ EXPLAINER_INTERVAL_MS: '0' }) }).explainerIntervalMs, 0);
  assert.equal(loadConfig({ env: baseEnv() }).explainerIntervalMs, 20 * 60 * 1000);
  // Still not a place for nonsense: negative and non-numeric are rejected as before.
  for (const bad of ['-1', 'never']) {
    assert.throws(
      () => loadConfig({ env: baseEnv({ EXPLAINER_INTERVAL_MS: bad }) }),
      (error: unknown) => {
        assert.ok(error instanceof ConfigError);
        assert.ok(error.problems.some((p) => p.includes('EXPLAINER_INTERVAL_MS')));
        return true;
      },
    );
  }
});
