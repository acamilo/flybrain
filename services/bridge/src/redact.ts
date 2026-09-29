/**
 * Keeps OAuth secrets out of the journal.
 *
 * `@twurple/api-call`'s `HttpStatusCodeError` puts the FULL request URL, query string included, in
 * its message — and twurple sends the refresh grant as a query string, so a failed refresh throws
 * an error whose message carries `client_secret=` and `refresh_token=` in clear. Anything that
 * prints a Twitch error (the entrypoint's last-resort handler, the EventSub failure lines, a failed
 * token persist) goes through here first.
 */
import { inspect } from 'node:util';

const SECRET_PARAMS = /\b(client_secret|refresh_token|access_token|code|token)=([^&\s"']+)/g;
const AUTH_HEADER = /\b(OAuth|Bearer)\s+[A-Za-z0-9._~+/-]{8,}/g;

export function redactSecrets(text: string): string {
  return text.replace(SECRET_PARAMS, '$1=<redacted>').replace(AUTH_HEADER, '$1 <redacted>');
}

/** One printable, secret-free description of anything thrown (stack and `cause` chain included). */
export function safeErrorText(error: unknown): string {
  return redactSecrets(error instanceof Error ? inspect(error) : String(error));
}

/** Like `safeErrorText`, but only the message: for one-line log entries. */
export function safeErrorMessage(error: unknown): string {
  return redactSecrets(error instanceof Error ? error.message : String(error));
}
