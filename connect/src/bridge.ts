// The page's side of the hand-off to galata-desk: three calls to the server
// that served this page, each carrying the attempt's one-time token.

/** The attempt is over: it succeeded elsewhere, was cancelled, or expired. */
export class AttemptEnded extends Error {}

/** The app refused the proof. `code` is `mismatch`, `bad_signature` or `bad_address`. */
export class ProofRefused extends Error {
  constructor(
    readonly code: string,
    detail: string,
  ) {
    super(detail);
  }
}

export interface AttemptInfo {
  /** The WalletConnect relay project, or null when none is configured. */
  projectId: string | null;
}

export class Bridge {
  constructor(private readonly token: string) {}

  /** Read the token from the URL, then take it out of the address bar. */
  static fromLocation(): Bridge | null {
    const token = new URLSearchParams(location.search).get('t');
    if (!token) return null;
    history.replaceState(null, '', location.pathname);
    return new Bridge(token);
  }

  private async call(path: string, body?: unknown): Promise<Response> {
    let response: Response;
    try {
      response = await fetch(path, {
        method: body === undefined ? 'GET' : 'POST',
        headers: {
          'x-desk-token': this.token,
          ...(body === undefined ? {} : { 'content-type': 'application/json' }),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        cache: 'no-store',
      });
    } catch {
      // The server has stopped: the attempt is over.
      throw new AttemptEnded();
    }
    if (response.status === 403) throw new AttemptEnded();
    return response;
  }

  async attempt(): Promise<AttemptInfo> {
    const response = await this.call('api/attempt');
    if (!response.ok) throw new AttemptEnded();
    return (await response.json()) as AttemptInfo;
  }

  /** The text the wallet must sign for `address`. */
  async challenge(address: string): Promise<string> {
    const response = await this.call('api/challenge', { address });
    if (!response.ok) await refuse(response);
    return ((await response.json()) as { message: string }).message;
  }

  /** Hand the proof to the app. Resolves with the address it accepted. */
  async prove(address: string, signature: string): Promise<string> {
    const response = await this.call('api/proof', { address, signature });
    if (!response.ok) await refuse(response);
    return ((await response.json()) as { address: string }).address;
  }
}

async function refuse(response: Response): Promise<never> {
  const body = (await response.json().catch(() => ({}))) as { error?: string; detail?: string };
  throw new ProofRefused(body.error ?? 'refused', body.detail ?? 'The app refused the request.');
}
