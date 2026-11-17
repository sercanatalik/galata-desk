// Finding wallets and asking them for two things: an account, and a signature.
//
// Browser extensions announce themselves through EIP-6963, so Rabby, MetaMask
// and any other installed wallet appear by their own name and icon, and two
// installed at once do not fight over `window.ethereum`.

/** The part of EIP-1193 this page uses. */
export interface Provider {
  request(args: { method: string; params?: unknown[] }): Promise<unknown>;
}

export interface Wallet {
  /** Stable for the wallet across page loads. */
  id: string;
  name: string;
  /** A data URI supplied by the wallet itself; absent for the legacy fallback. */
  icon?: string;
  provider: Provider;
}

interface Announcement extends Event {
  detail: {
    info: { uuid: string; name: string; icon: string; rdns: string };
    provider: Provider;
  };
}

/** The user said no in the wallet (EIP-1193 code 4001). */
export class Declined extends Error {}

/**
 * Collect the wallets installed in this browser. `onChange` is called with the
 * list each time another announces itself; extensions may answer late.
 */
export function discover(onChange: (wallets: Wallet[]) => void): void {
  const found = new Map<string, Wallet>();

  window.addEventListener('eip6963:announceProvider', (event) => {
    const { info, provider } = (event as Announcement).detail;
    // Only image data the wallet embedded; never a URL to fetch.
    const icon = info.icon.startsWith('data:image/') ? info.icon : undefined;
    found.set(info.rdns || info.uuid, { id: info.rdns || info.uuid, name: info.name, icon, provider });
    onChange([...found.values()]);
  });
  window.dispatchEvent(new Event('eip6963:requestProvider'));

  // A wallet that predates EIP-6963 only sets `window.ethereum`.
  setTimeout(() => {
    const legacy = (window as { ethereum?: Provider }).ethereum;
    if (found.size === 0 && legacy) {
      found.set('legacy', { id: 'legacy', name: 'Browser wallet', provider: legacy });
      onChange([...found.values()]);
    }
  }, 400);
}

function rethrow(error: unknown): never {
  const code = (error as { code?: number } | null)?.code;
  if (code === 4001 || code === 5000) throw new Declined();
  throw error instanceof Error ? error : new Error(String(error));
}

/** Ask the wallet to share an account. Returns its address. */
export async function requestAccount(provider: Provider): Promise<string> {
  try {
    const accounts = (await provider.request({ method: 'eth_requestAccounts' })) as string[];
    const account = accounts[0];
    if (!account) throw new Error('The wallet shared no account.');
    return account;
  } catch (error) {
    rethrow(error);
  }
}

/** Ask the wallet to sign `message` as `address` with `personal_sign`. */
export async function signMessage(provider: Provider, address: string, message: string): Promise<string> {
  const bytes = new TextEncoder().encode(message);
  const hex = '0x' + [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');
  try {
    return (await provider.request({ method: 'personal_sign', params: [hex, address] })) as string;
  } catch (error) {
    rethrow(error);
  }
}
