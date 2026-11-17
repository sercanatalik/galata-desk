// Phone wallets: a WalletConnect session, started by scanning a QR code.
//
// The SDK is loaded only when the user asks for the code, so a visit that
// uses a browser extension never opens a connection to the relay.

import type { Provider } from './wallets';

export interface PhoneSession {
  provider: Provider;
  address: string;
  /** End the session, so the wallet does not keep a connection it has no use for. */
  close(): Promise<void>;
}

/**
 * Start a session. `onUri` receives the pairing URI to show as a QR code.
 * Resolves once a wallet has scanned it and approved.
 */
export async function connectPhone(projectId: string, onUri: (uri: string) => void): Promise<PhoneSession> {
  const { EthereumProvider } = await import('@walletconnect/ethereum-provider');
  const provider = await EthereumProvider.init({
    projectId,
    // Signing a message needs no chain; these are only what a wallet may offer.
    optionalChains: [1, 42161],
    optionalMethods: ['personal_sign'],
    showQrModal: false,
    metadata: {
      name: 'galata-desk',
      description: 'Shows the Hyperliquid portfolio of an account you own. Read-only.',
      url: location.origin,
      icons: [],
    },
  });
  provider.on('display_uri', onUri);
  await provider.connect();
  const address = provider.accounts[0];
  if (!address) {
    await provider.disconnect().catch(() => {});
    throw new Error('The wallet shared no account.');
  }
  return {
    provider: provider as unknown as Provider,
    address,
    close: () => provider.disconnect().catch(() => {}),
  };
}
