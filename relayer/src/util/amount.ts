/**
 * Pi Network uses the same Horizon API amount format as Stellar: a decimal
 * string with exactly 7 fractional digits (e.g. "3.1415926"). One native Pi
 * equals 10^7 stroops.
 *
 * Source: Pi Network Horizon API `/operations` amount field; matches Stellar's
 * documented stroops model — see
 * https://developers.stellar.org/docs/learn/fundamentals/stellar-data-structures/assets#amount-precision
 */
export const PI_DECIMALS = 7;
export const STROOPS_PER_PI = 10n ** BigInt(PI_DECIMALS);

/** Converts a Horizon-style decimal amount string (e.g. "12.5000000") to stroops. */
export function decimalToStroops(decimal: string): bigint {
  const parts = decimal.split('.');
  if (parts.length > 2) {
    throw new Error(`Invalid decimal amount: ${decimal}`);
  }
  const whole = parts[0] ?? '';
  const fraction = parts[1] ?? '';
  if (!/^\d+$/.test(whole) || !/^\d*$/.test(fraction)) {
    throw new Error(`Invalid decimal amount: ${decimal}`);
  }
  const paddedFraction = fraction.padEnd(PI_DECIMALS, '0').slice(0, PI_DECIMALS);
  return BigInt(whole) * STROOPS_PER_PI + BigInt(paddedFraction || '0');
}

/**
 * Decodes a Horizon-style paging token (TOID) into its ledger sequence.
 * TOIDs pack `ledger_sequence << 32 | tx_order << 12 | op_order`.
 */
export function ledgerFromPagingToken(pagingToken: string): number {
  return Number(BigInt(pagingToken) >> 32n);
}

/** Converts stroops to a Horizon-style decimal amount string (e.g. "12.5000000"). */
export function stroopsToDecimal(stroops: bigint): string {
  const whole = stroops / STROOPS_PER_PI;
  const fraction = stroops % STROOPS_PER_PI;
  return `${whole}.${fraction.toString().padStart(PI_DECIMALS, '0')}`;
}
