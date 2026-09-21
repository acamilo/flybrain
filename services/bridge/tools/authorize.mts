#!/usr/bin/env -S npx tsx
/**
 * One-time interactive authorization-code flow (`docs/design/stage-bridge.md` B1), run by hand
 * on the WSL box to produce `tokens.json`. Twitch permits `http://localhost` redirects, so this
 * spins up a temporary listener on `http://localhost:3000/callback`, prints the authorize URL to
 * open in a browser, and on the callback exchanges the code for tokens via `exchangeCode` and
 * writes them into `tokens.json` (atomic, 0600).
 *
 * Run once per identity — bot and broadcaster are different Twitch accounts, so you log in as
 * each in turn:
 *
 * ```sh
 * TWITCH_CLIENT_ID=... TWITCH_CLIENT_SECRET=... \
 *   npx tsx tools/authorize.mts --role bot --tokens-file /var/lib/flybridge/tokens.json
 * TWITCH_CLIENT_ID=... TWITCH_CLIENT_SECRET=... \
 *   npx tsx tools/authorize.mts --role broadcaster --tokens-file /var/lib/flybridge/tokens.json \
 *   --with-redemptions --with-predictions
 * ```
 *
 * `--with-redemptions` / `--with-predictions` add the scopes those features need
 * (`src/scopes.ts`'s `SCOPE_TABLE`) even though B1 ships with both features off, so the token
 * does not need re-authorizing the day they're switched on.
 *
 * Device-code grant (`https://id.twitch.tv/oauth2/device`) is the documented fallback for
 * headless authorization; twurple's first-class support for it is unverified as of this writing,
 * so it is not implemented here — do the one-time flow from a machine with a browser instead.
 */
import { createServer } from 'node:http';
import { exchangeCode, getTokenInfo } from '@twurple/auth';
import { atomicWriteJson } from '../src/atomic-file';
import { SCOPE_TABLE, type TokenKind } from '../src/scopes';
import type { StoredToken, TokensFile } from '../src/auth';
import { readFile } from 'node:fs/promises';

const REDIRECT_URI = 'http://localhost:3000/callback';
const CALLBACK_PORT = 3000;

interface CliArgs {
  role: TokenKind;
  tokensFile: string;
  withRedemptions: boolean;
  withPredictions: boolean;
}

function parseArgs(argv: string[]): CliArgs {
  const args: CliArgs = {
    role: 'bot',
    tokensFile: process.env.TOKENS_FILE ?? '/var/lib/flybridge/tokens.json',
    withRedemptions: false,
    withPredictions: false,
  };
  for (let i = 0; i < argv.length; i++) {
    switch (argv[i]) {
      case '--role': {
        const value = argv[++i];
        if (value !== 'bot' && value !== 'broadcaster') throw new Error('--role must be "bot" or "broadcaster"');
        args.role = value;
        break;
      }
      case '--tokens-file':
        args.tokensFile = argv[++i] ?? args.tokensFile;
        break;
      case '--with-redemptions':
        args.withRedemptions = true;
        break;
      case '--with-predictions':
        args.withPredictions = true;
        break;
      default:
        throw new Error(`unknown argument: ${argv[i]}`);
    }
  }
  return args;
}

function scopesForRole(role: TokenKind, withRedemptions: boolean, withPredictions: boolean): string[] {
  const scopes = SCOPE_TABLE.filter((req) => req.token === role).filter(
    (req) => req.requiredWhen({ featureRedemptions: withRedemptions, featurePredictions: withPredictions }),
  ).map((req) => req.scope);
  return [...new Set(scopes)];
}

function waitForAuthorizationCode(clientId: string, scopes: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    const server = createServer((req, res) => {
      const url = new URL(req.url ?? '/', REDIRECT_URI);
      if (url.pathname !== '/callback') {
        res.writeHead(404).end('not found');
        return;
      }
      const error = url.searchParams.get('error');
      if (error) {
        res.writeHead(400).end(`authorization failed: ${error}`);
        server.close();
        reject(new Error(`Twitch returned an error: ${error} (${url.searchParams.get('error_description') ?? ''})`));
        return;
      }
      const code = url.searchParams.get('code');
      if (!code) {
        res.writeHead(400).end('missing code');
        return;
      }
      res.writeHead(200, { 'content-type': 'text/plain' }).end('Authorized. You can close this tab.');
      server.close();
      resolve(code);
    });

    server.listen(CALLBACK_PORT, '127.0.0.1', () => {
      const authorizeUrl = new URL('https://id.twitch.tv/oauth2/authorize');
      authorizeUrl.searchParams.set('client_id', clientId);
      authorizeUrl.searchParams.set('redirect_uri', REDIRECT_URI);
      authorizeUrl.searchParams.set('response_type', 'code');
      authorizeUrl.searchParams.set('scope', scopes.join(' '));
      console.log('Open this URL and authorize with the correct Twitch account:\n');
      console.log(`  ${authorizeUrl.toString()}\n`);
      console.log(`Waiting for the callback on ${REDIRECT_URI} ...`);
    });

    server.once('error', reject);
  });
}

async function loadExistingTokens(path: string): Promise<Partial<TokensFile>> {
  try {
    return JSON.parse(await readFile(path, 'utf8')) as Partial<TokensFile>;
  } catch (cause) {
    if ((cause as NodeJS.ErrnoException).code === 'ENOENT') return {};
    throw cause;
  }
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  const clientId = process.env.TWITCH_CLIENT_ID;
  const clientSecret = process.env.TWITCH_CLIENT_SECRET;
  if (!clientId || !clientSecret) {
    throw new Error('set TWITCH_CLIENT_ID and TWITCH_CLIENT_SECRET in the environment before running this tool');
  }

  const scopes = scopesForRole(args.role, args.withRedemptions, args.withPredictions);
  console.log(`Requesting scopes for "${args.role}": ${scopes.join(', ')}`);

  const code = await waitForAuthorizationCode(clientId, scopes);
  const accessToken = await exchangeCode(clientId, clientSecret, code, REDIRECT_URI);
  const info = await getTokenInfo(accessToken.accessToken, clientId);
  if (!info.userId) throw new Error('Twitch did not return a user id for this token');

  const stored: StoredToken = {
    userId: info.userId,
    accessToken: accessToken.accessToken,
    refreshToken: accessToken.refreshToken,
    scope: accessToken.scope,
    expiresIn: accessToken.expiresIn,
    obtainmentTimestamp: accessToken.obtainmentTimestamp,
  };

  const existing = await loadExistingTokens(args.tokensFile);
  const merged: Partial<TokensFile> = { ...existing, [args.role]: stored };
  await atomicWriteJson(args.tokensFile, merged);

  console.log(`\nAuthorized as ${info.userName ?? info.userId} (${args.role}).`);
  console.log(`Wrote ${args.tokensFile} (0600).`);
  if (!merged.bot || !merged.broadcaster) {
    const missingRole: TokenKind = args.role === 'bot' ? 'broadcaster' : 'bot';
    console.log(`Still missing "${missingRole}" — run this tool again with --role ${missingRole}.`);
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
