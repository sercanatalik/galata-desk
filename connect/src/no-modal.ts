// The WalletConnect provider can show Reown's own modal, and imports it on
// demand. This page draws its own QR code and never asks for that modal, so
// the import is pointed here and its several hundred kilobytes are left out.

export function createAppKit(): never {
  throw new Error('The WalletConnect modal is not part of this page.');
}
