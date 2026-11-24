# Risk

How much your open positions could lose in a day, which positions that risk
comes from, how much of it is one bet on a benchmark, and which positions move
together. The section only reads data: it has no control that trades.

Every figure is for a **1-day horizon**, in USD, from your open perpetual
positions at their current mark.

## Choices

- **Focused period**: 30D, 90D, 180D or 1Y. The matrix, the breakdown and the
  Beta & clusters view use it.
- **Periods compared**: the lookbacks listed side by side in **Periods
  compared**.
- **Estimator**: how the covariance of returns is estimated.
  - **EWMA λ0.94** (default) weighs recent returns most, so it reacts when
    markets turn.
  - **Sample** weighs every return in the window the same.
  - **Ledoit-Wolf** pulls every pair's correlation toward the average. Use it
    when you hold many positions and the window has few returns.
- **Confidence**: 95% or 99%.

## VaR & covariance

- **Parametric VaR**: the loss a normal distribution of returns exceeds on
  one day in 20 (95%) or one in 100 (99%).
- **Cornish-Fisher VaR**: the same, adjusted for the skew and fat tails of
  your book's own returns. Where the adjustment breaks down, the card says the
  normal figure is used.
- **Historical VaR** and **Expected shortfall**: the loss at that confidence
  among the period's actual returns, replayed at today's positions, and the
  average loss beyond it. The card says how many returns fall in the tail. At
  99% over 90 returns that is one, so read it as a single bad day rather than
  a statistic.
- **Correlation / Covariance**: the matrix of your positions' returns. Blue
  cells move together, orange cells move against each other.
- **VaR by position**: each position's share of parametric VaR. The shares add
  up to the total. A negative share means the position hedges the rest of the
  book. **per $100** is how much VaR another $100 of the position would add.
- **Periods compared**: for each lookback, its candle size, how many returns
  it used, and its VaR figures.

### Scaled figures

Each lookback uses the candles Markets loads for it: 8-hour candles for 30D,
daily candles for 90D and 180D, and 3-day candles for 1Y. Figures from 8-hour
and 3-day candles are converted to one day by the square root of time and
marked **×√3** or **÷√3**. That conversion assumes calm, independent returns,
so with jumps and fat tails it understates risk. The EWMA decay applies per
candle, so its memory is shorter in days for 30D and longer for 1Y.

## Beta & clusters

- **Benchmark**: any held market. BTC is the default when you hold it,
  otherwise your largest position. Your choice is remembered, even while you
  don't hold it.
- **Beta** is how much a position moves when the benchmark moves 1%. A
  position's **equivalent** is beta times its size: how much of the benchmark
  it behaves like, in USD and in benchmark units.
- **β-adjusted gross and net** add up those equivalents. **Hedge to
  β-neutral** is the benchmark trade that cancels the net.
- **R²** is how much of a position's movement the benchmark explains.
  **Residual σ** is the risk a benchmark hedge would leave. Below 0.25 both
  are amber: a small equivalent there means the position does not follow the
  benchmark, not that it is safe.
- **Correlated clusters** group positions whose correlation is at or above
  0.6, 0.7 or 0.8, directly or through another position. A cluster's gross is
  how big that single combined bet really is.

## HIP-3 markets

Markets on a builder dex, such as `xyz:SILVER` or `xyz:NVDA`, are priced by an
oracle that stops moving while the underlying market is closed. Their returns
are flat at weekends and jump when the underlying reopens, which pulls their
correlations toward zero. They are marked **HIP-3** wherever they appear.

## Loading

Candle history loads within the same request allowance as Markets, the
focused period first. Until a period has loaded its figures show as loading,
never as zero. A market whose history cannot be loaded is named at the top and
left out of every figure until it loads.
