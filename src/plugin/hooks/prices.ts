// What a model's tokens cost, for the agents panel's estimate (SKEIN-1361).
//
// One price table in skein, not two: this is `PRICES` in `src/usage.rs`, row for row, and
// `the_mods_price_table_is_usages` there holds the two equal, `PRICES_AS_OF` included. Change a
// price there and here in the same commit. The rules are usage.rs's too:
//
// - A model is matched exactly, then by its longest known prefix that ends at a `-`, so a dated
//   snapshot (`claude-haiku-4-5-20251001`) takes its model's price and `claude-opus-50` never
//   takes `claude-opus-5`'s.
// - A model the table does not know has no price. It is never priced at zero, and never at a
//   guess: the panel says the cost is not known.
// - The engine reports one cache-write count with no split by lifetime. usage.rs prices a write
//   it cannot attribute at the 5-minute rate, the cheaper one, so an unattributed token never
//   inflates a figure. So does this. A session that writes 1-hour entries costs more than shown.
//
// Each row: model, then dollars per million tokens for input, output, cache read, cache write
// (5 minutes), cache write (1 hour). Keep one row per line, in this form: the Rust test reads it.

export const PRICES_AS_OF = '2026-10-30'

export type Price = {
  input: number
  output: number
  cacheRead: number
  cacheWrite5m: number
  cacheWrite1h: number
}

type Row = readonly [model: string, input: number, output: number, cacheRead: number, cacheWrite5m: number, cacheWrite1h: number]

const PRICES: readonly Row[] = [
  ['claude-opus-5-5', 4, 20, 0.2, 5, 8],
  ['claude-opus-5', 5, 25, 0.5, 6.25, 10],
  ['claude-opus-4-8', 5, 25, 0.5, 6.25, 10],
  ['claude-opus-4-7', 5, 25, 0.5, 6.25, 10],
  ['claude-opus-4-6', 5, 25, 0.5, 6.25, 10],
  ['claude-sonnet-5-5', 2, 10, 0.2, 2.5, 4],
  ['claude-sonnet-5', 2, 10, 0.2, 2.5, 4],
  ['claude-sonnet-4-6', 3, 15, 0.3, 3.75, 6],
  ['claude-haiku-4-5', 1, 5, 0.1, 1.25, 2],
  ['claude-fable-5', 10, 50, 1, 12.5, 20],
  ['claude-fable-5-1', 10, 50, 0.25, 12.5, 20],
]

/** The usage of one request or one turn, as the engine reports it. */
export type Usage = {
  input_tokens: number
  output_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
}

const priceRow = (row: Row): Price => ({
  input: row[1],
  output: row[2],
  cacheRead: row[3],
  cacheWrite5m: row[4],
  cacheWrite1h: row[5],
})

/** The price of a model id, or null when the table does not know it. */
export function priceOf(model: string): Price | null {
  const exact = PRICES.find(row => row[0] === model)
  if (exact) return priceRow(exact)
  let best: Row | null = null
  for (const row of PRICES) {
    const isPrefix = model.length > row[0].length && model.startsWith(row[0]) && model[row[0].length] === '-'
    if (isPrefix && (best === null || row[0].length > best[0].length)) best = row
  }

  return best === null ? null : priceRow(best)
}

/** What the usage cost in dollars, or null when the model has no price. */
export function costOf(model: string, usage: Usage): number | null {
  const price = priceOf(model)
  if (price === null) return null

  return (
    ((usage.input_tokens || 0) * price.input +
      (usage.output_tokens || 0) * price.output +
      (usage.cache_read_input_tokens || 0) * price.cacheRead +
      (usage.cache_creation_input_tokens || 0) * price.cacheWrite5m) /
    1_000_000
  )
}
