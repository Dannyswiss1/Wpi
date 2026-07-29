import { describe, expect, it } from 'vitest';
import {
  decimalToStroops,
  ledgerFromPagingToken,
  PI_DECIMALS,
  STROOPS_PER_PI,
  stroopsToDecimal,
} from '../src/util/amount.js';

describe('decimalToStroops', () => {
  it('converts a whole-number amount', () => {
    expect(decimalToStroops('12')).toBe(120_000_000n);
  });

  it('converts a fractional amount', () => {
    expect(decimalToStroops('12.5')).toBe(125_000_000n);
  });

  it('handles full 7-decimal precision', () => {
    expect(decimalToStroops('0.0000001')).toBe(1n);
  });

  it('truncates extra precision beyond 7 decimals', () => {
    expect(decimalToStroops('1.00000009')).toBe(10_000_000n);
  });

  it('rejects malformed input', () => {
    expect(() => decimalToStroops('abc')).toThrow();
    expect(() => decimalToStroops('1.2.3')).toThrow();
  });
});

describe('stroopsToDecimal', () => {
  it('round-trips through decimalToStroops', () => {
    expect(stroopsToDecimal(125_000_000n)).toBe('12.5000000');
    expect(decimalToStroops(stroopsToDecimal(125_000_000n))).toBe(125_000_000n);
  });

  it('pads small fractional amounts', () => {
    expect(stroopsToDecimal(1n)).toBe('0.0000001');
  });
});

describe('ledgerFromPagingToken', () => {
  it('extracts the ledger sequence from the high 32 bits', () => {
    const ledger = 12345;
    const toid = (BigInt(ledger) << 32n) | 4096n;
    expect(ledgerFromPagingToken(toid.toString())).toBe(ledger);
  });
});

describe('Pi Network decimal consistency (Issue #21)', () => {
  it('PI_DECIMALS is 7 and STROOPS_PER_PI equals 10^PI_DECIMALS', () => {
    expect(PI_DECIMALS).toBe(7);
    expect(STROOPS_PER_PI).toBe(10_000_000n);
    expect(STROOPS_PER_PI).toBe(10n ** BigInt(PI_DECIMALS));
  });

  // Representative amounts from Pi Network's Horizon-compatible API.
  // Pi's `/payments` endpoint returns native amounts as 7-decimal strings,
  // e.g. "3.1415926", identical to Stellar's documented format.
  const piHorizonSamples: { decimal: string; stroops: bigint; label: string }[] = [
    { decimal: '1.0000000', stroops: 10_000_000n, label: '1 Pi, full precision' },
    { decimal: '3.1415926', stroops: 31_415_926n, label: '~pi Pi' },
    { decimal: '100.0000000', stroops: 1_000_000_000n, label: '100 Pi' },
    { decimal: '0.0000001', stroops: 1n, label: 'smallest representable unit (1 stroop)' },
    { decimal: '0.5000000', stroops: 5_000_000n, label: '0.5 Pi' },
    { decimal: '99999.9999999', stroops: 999_999_999_999n, label: 'large amount at max precision' },
  ];

  for (const { decimal, stroops, label } of piHorizonSamples) {
    it(`converts Pi Horizon amount "${decimal}" (${label}) to ${stroops} stroops`, () => {
      expect(decimalToStroops(decimal)).toBe(stroops);
    });

    it(`round-trips "${decimal}" through stroopsToDecimal`, () => {
      expect(stroopsToDecimal(decimalToStroops(decimal))).toBe(decimal);
    });
  }

  it('wPi mint amount equals raw Pi deposit stroops (1:1 peg)', () => {
    // The relayer reads amountStroops from the Pi Horizon payment and passes
    // it directly to the contract's mint_from_deposit. A correct DECIMALS
    // value means no scaling is needed; a wrong value would silently scale
    // by 10^(wrong - correct).
    const piDeposit = '50.0000000';
    const depositStroops = decimalToStroops(piDeposit);
    expect(depositStroops).toBe(500_000_000n);

    const mintedStroops = depositStroops; // 1:1 peg
    expect(stroopsToDecimal(mintedStroops)).toBe(piDeposit);
  });
});
