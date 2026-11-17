// The connect page: pick a wallet, sign one message, done.
//
// It asks the wallet for exactly two things, an account and a signature over
// text the user can read, and hands both to galata-desk on this machine.

import '@fontsource-variable/instrument-sans';
import '@fontsource-variable/jetbrains-mono';
import QRCode from 'qrcode';

import { AttemptEnded, Bridge, ProofRefused } from './bridge';
import './style.css';
import { connectPhone } from './walletconnect';
import { Declined, discover, requestAccount, signMessage, type Provider, type Wallet } from './wallets';

const app = document.getElementById('app')!;

/** Build an element. Text is always set as text, never as markup. */
function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: { class?: string; text?: string } = {},
  ...children: (Node | null)[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (props.class) node.className = props.class;
  if (props.text !== undefined) node.textContent = props.text;
  for (const child of children) if (child) node.append(child);
  return node;
}

function button(label: string, onClick: () => void, primary = false): HTMLButtonElement {
  const node = el('button', { class: primary ? 'button primary' : 'button', text: label });
  node.type = 'button';
  node.addEventListener('click', onClick);
  return node;
}

function short(address: string): string {
  return `${address.slice(0, 6)}…${address.slice(-4)}`;
}

/** The Galata Tower: the family's mark, the same shape as assets/logo.svg. */
function tower(): SVGSVGElement {
  const ns = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(ns, 'svg');
  svg.setAttribute('viewBox', '0 0 64 64');
  svg.setAttribute('aria-hidden', 'true');
  const lantern = document.createElementNS(ns, 'circle');
  lantern.setAttribute('cx', '32');
  lantern.setAttribute('cy', '4.2');
  lantern.setAttribute('r', '1.6');
  const body = document.createElementNS(ns, 'path');
  body.setAttribute('fill-rule', 'evenodd');
  body.setAttribute(
    'd',
    'M32 6.5L46.5 25.5H44.5V34H42V56H46V60H18V56H22V34H19.5V25.5H17.5ZM24.2 32V29.4A1.8 1.8 0 0 1 27.8 29.4V32ZM30.2 32V29.4A1.8 1.8 0 0 1 33.8 29.4V32ZM36.2 32V29.4A1.8 1.8 0 0 1 39.8 29.4V32ZM30.8 42V38.6A1.2 1.2 0 0 1 33.2 38.6V42ZM29.6 47.5A2.4 2.4 0 0 1 34.4 47.5A2.4 2.4 0 0 1 33.38 49.47L34.3 55H29.7L30.62 49.47A2.4 2.4 0 0 1 29.6 47.5Z',
  );
  svg.append(lantern, body);
  return svg;
}

function frame(...body: Node[]): void {
  app.replaceChildren(
    el('div', { class: 'wordmark' }, tower(), el('span', { text: 'galata-desk' })),
    el('h1', { text: 'Connect a wallet' }),
    el('p', {
      class: 'lede',
      text: 'Prove you own a Hyperliquid account so galata-desk can show its portfolio. Read-only: nothing here can trade or move funds.',
    }),
    ...body,
    el('footer', {
      text: 'You will be asked to sign one plain-text message. It is not a transaction, costs nothing, and is checked on this computer only.',
    }),
  );
}

/** A panel that says what is happening or what went wrong. */
function status(
  kind: 'working' | 'problem' | 'done',
  title: string,
  detail: string,
  ...actions: HTMLButtonElement[]
): HTMLElement {
  const heading = el('p', { class: 'title', text: title });
  return el(
    'section',
    { class: `panel status ${kind === 'working' ? '' : kind}` },
    kind === 'working' ? el('div', { class: 'row' }, el('div', { class: 'spinner' }), heading) : heading,
    el('p', { text: detail }),
    actions.length ? el('div', { class: 'actions' }, ...actions) : null,
  );
}

function ended(): void {
  frame(
    status(
      'problem',
      'This connection attempt has ended',
      'It was completed, cancelled, or it expired after five minutes. Start again from galata-desk.',
    ),
  );
}

async function main(): Promise<void> {
  const bridge = Bridge.fromLocation();
  if (!bridge) return ended();

  let projectId: string | null;
  try {
    projectId = (await bridge.attempt()).projectId;
  } catch {
    return ended();
  }

  let wallets: Wallet[] = [];
  let choosing = true;

  /** Get the signature for `address` from `provider` and hand it to the app. */
  async function prove(provider: Provider, address: string, walletName: string, after?: () => Promise<void>) {
    choosing = false;
    try {
      const message = await bridge!.challenge(address);
      frame(
        status(
          'working',
          `Sign the message in ${walletName}`,
          `${walletName} is asking you to sign a short message as ${short(address)}. Read it, then approve.`,
        ),
      );
      const signature = await signMessage(provider, address, message);
      const accepted = await bridge!.prove(address, signature);
      await after?.();
      const done = status(
        'done',
        'Connected',
        'You can close this tab and return to galata-desk.',
      );
      done.insertBefore(el('p', { class: 'address', text: short(accepted) }), done.children[1] ?? null);
      frame(done);
    } catch (error) {
      if (error instanceof AttemptEnded) {
        await after?.();
        return ended();
      }
      const retry = button('Try again', () => void prove(provider, address, walletName, after), true);
      const back = button('Choose another wallet', () => {
        void after?.();
        choose();
      });
      if (error instanceof Declined) {
        frame(
          status(
            'problem',
            'You declined to sign',
            'galata-desk needs the signature to know the account is yours. Nothing was shared.',
            retry,
            back,
          ),
        );
      } else if (error instanceof ProofRefused) {
        frame(
          status(
            'problem',
            'The signature did not match',
            `galata-desk could not confirm that ${short(address)} signed the message. Make sure the wallet is on that account, then try again.`,
            retry,
            back,
          ),
        );
      } else {
        frame(
          status(
            'problem',
            'The wallet did not answer',
            error instanceof Error ? error.message : 'Something went wrong in the wallet.',
            retry,
            back,
          ),
        );
      }
    }
  }

  async function useExtension(wallet: Wallet) {
    choosing = false;
    frame(status('working', `Waiting for ${wallet.name}`, `Approve the connection in ${wallet.name}.`));
    try {
      const address = await requestAccount(wallet.provider);
      await prove(wallet.provider, address, wallet.name);
    } catch (error) {
      const title = error instanceof Declined ? 'You declined to connect' : `${wallet.name} did not answer`;
      const detail =
        error instanceof Declined
          ? 'Nothing was shared.'
          : error instanceof Error
            ? error.message
            : 'Something went wrong in the wallet.';
      frame(
        status(
          'problem',
          title,
          detail,
          button('Try again', () => void useExtension(wallet), true),
          button('Choose another wallet', choose),
        ),
      );
    }
  }

  async function usePhone(id: string) {
    choosing = false;
    frame(status('working', 'Preparing a QR code', 'Contacting the WalletConnect relay.'));
    try {
      const session = await connectPhone(id, (uri) => {
        void QRCode.toDataURL(uri, { margin: 0, width: 464, errorCorrectionLevel: 'M' }).then((src) => {
          const image = el('img');
          image.src = src;
          image.alt = 'WalletConnect QR code';
          frame(
            el('h2', { text: 'Scan with your phone' }),
            el(
              'section',
              { class: 'panel qr' },
              image,
              el('p', {
                text: 'Open MetaMask, Rabby or another wallet on your phone, choose its scan or WalletConnect option, and point it at this code.',
              }),
              button('Choose another wallet', choose),
            ),
          );
        });
      });
      await prove(session.provider, session.address, 'your phone wallet', session.close);
    } catch (error) {
      frame(
        status(
          'problem',
          'The phone wallet did not connect',
          error instanceof Error && error.message ? error.message : 'The connection was rejected or timed out.',
          button('Show a new code', () => void usePhone(id), true),
          button('Choose another wallet', choose),
        ),
      );
    }
  }

  function choose() {
    choosing = true;
    const list = el('section', { class: 'panel' });
    if (wallets.length === 0) {
      list.append(
        el('p', {
          class: 'note',
          text: 'No wallet extension was found in this browser. Install Rabby or MetaMask, or open this page in the browser that has it.',
        }),
      );
    }
    for (const wallet of wallets) {
      const item = el('button', { class: 'wallet' });
      item.type = 'button';
      if (wallet.icon) {
        const icon = el('img');
        icon.src = wallet.icon;
        icon.alt = '';
        item.append(icon);
      } else {
        item.append(el('span', { class: 'glyph', text: wallet.name.slice(0, 1) }));
      }
      item.append(el('span', { class: 'name', text: wallet.name }), el('span', { class: 'hint', text: 'Extension' }));
      item.addEventListener('click', () => void useExtension(wallet));
      list.append(item);
    }

    const phone = el('section', { class: 'panel' });
    if (projectId) {
      const item = el('button', { class: 'wallet' });
      item.type = 'button';
      item.append(
        el('span', { class: 'glyph', text: 'QR' }),
        el('span', { class: 'name', text: 'Scan a QR code' }),
        el('span', { class: 'hint', text: 'WalletConnect' }),
      );
      item.addEventListener('click', () => void usePhone(projectId!));
      phone.append(item);
    } else {
      const note = el('p', { class: 'note' });
      note.append(
        'Phone wallets connect through WalletConnect, which needs a relay project id. Set ',
        el('code', { text: 'reown_project_id' }),
        ' in galata-desk’s ',
        el('code', { text: 'config.toml' }),
        ', or the ',
        el('code', { text: 'GALATA_DESK_REOWN_PROJECT_ID' }),
        ' environment variable, and connect again. Browser extensions work without it.',
      );
      phone.append(note);
    }

    frame(el('h2', { text: 'In this browser' }), list, el('h2', { text: 'On your phone' }), phone);
  }

  choose();
  discover((found) => {
    wallets = found;
    // Extensions may announce late; redraw only while the user is still choosing.
    if (choosing) choose();
  });
}

void main();
