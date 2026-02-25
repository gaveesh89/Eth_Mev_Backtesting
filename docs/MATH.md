# Integer Math Reference

> Describes every core formula used in the MEV backtest toolkit.
> All decision-path arithmetic uses `U256` / `u128` — no `f64` rounding.

---

## 1. Uniswap V2 `getAmountOut` (constant-product)

Given reserves $(R_{in}, R_{out})$ and fee fraction $\frac{f_{num}}{f_{den}}$ (default 997/1000):

$$
\text{amountOut} = \frac{x \cdot f_{num} \cdot R_{out}}{R_{in} \cdot f_{den} + x \cdot f_{num}}
$$

where $x$ is the input amount.

**Implementation:** [`arbitrage.rs` `get_amount_out`](../crates/mev-sim/src/strategies/arbitrage.rs) —
promotes every operand to `U256` before multiplication to avoid `u128` overflow.

**CEX-DEX variant:** [`cex_dex_arb.rs` `amount_out`](../crates/mev-sim/src/strategies/cex_dex_arb.rs) —
identical formula, standalone copy used in the candidate-sweep profit estimator.

---

## 2. DEX-DEX Spread (basis points, integer)

For two pools $A$ and $B$ quoting the same pair, the implied prices are
$p_A = R_{1,A} / R_{0,A}$ and $p_B = R_{1,B} / R_{0,B}$.

To avoid division, cross-multiply:

$$
P_A = R_{1,A} \cdot R_{0,B}, \qquad P_B = R_{1,B} \cdot R_{0,A}
$$

Spread in basis points (truncated):

$$
\text{spread\_bps} = \left\lfloor \frac{|P_A - P_B| \cdot 10\,000}{\min(P_A,\, P_B)} \right\rfloor
$$

**Implementation:** [`arbitrage.rs` `spread_bps_integer`](../crates/mev-sim/src/strategies/arbitrage.rs) —
all intermediate values are `U256`; result saturated to `u128`.

---

## 3. DEX-DEX Two-Leg Profit

### 3a. Closed-form optimal input (same-fee pools)

For pools with identical fee structures, the optimal input $x^*$ that maximises
two-leg profit ($\text{sell}_B(\text{buy}_A(x)) - x$) is:

$$
x^* = \frac{f_{num} \cdot \sqrt{R_{in,A} \cdot R_{out,A} \cdot R_{in,B} \cdot R_{out,B}}
            \;-\; f_{den} \cdot R_{in,A} \cdot R_{in,B}}
           {f_{num} \cdot R_{in,B} \cdot f_{den} + f_{num}^2 \cdot R_{out,A}}
$$

**Implementation:** [`arbitrage.rs` `optimal_input_closed_form`](../crates/mev-sim/src/strategies/arbitrage.rs) —
pre-computes `presqrt = f_num² × R_out_A × R_out_B / (R_in_A × R_in_B)`, takes integer
square root, checks feasibility ($\sqrt{\text{presqrt}} \ge f_{den}$), then substitutes.
If infeasible or reserves exceed Uniswap V2 `uint112` domain, falls back to ternary search.

### 3b. Ternary search fallback

When the closed-form is ineligible (mixed fee structures, out-of-range reserves),
ternary search over $[1, \text{max\_input}]$ (10% of $R_{in}$) finds the profit-maximising
input in $O(\log n)$ iterations.

**Implementation:** [`arbitrage.rs` `ternary_search_optimal_input`](../crates/mev-sim/src/strategies/arbitrage.rs).

### 3c. Net profit

$$
\text{net\_profit} = \text{gross\_profit} - \text{gas\_cost\_token0}
$$

Gas cost is converted from ETH wei using the WETH price pool when the input token
is not WETH itself. Computed via `convert_eth_wei_to_token0_wei`.

---

## 4. CEX-DEX Spread (basis points, integer)

Given DEX reserves pricing WETH:

$$
p_{\text{dex}} = \frac{R_{\text{quote}} \cdot 10^{18}}{R_{\text{weth}}}
$$

CEX price stored as fixed-point with 8 decimal places ($\text{fp} = \text{price} \times 10^8$),
converted to quote-token units:

$$
p_{\text{cex}} = \frac{\text{fp} \cdot 10^{d_q}}{10^{8}}
$$

where $d_q$ is the quote token's decimals (6 for USDC/USDT, 18 for DAI).

Spread:

$$
\text{spread\_bps} = \left\lfloor \frac{|p_{\text{dex}} - p_{\text{cex}}| \cdot 10\,000}{p_{\text{dex}}} \right\rfloor
$$

**Implementation:** [`cex_dex_arb.rs` `evaluate_cex_dex_opportunity`](../crates/mev-sim/src/strategies/cex_dex_arb.rs) —
all comparisons use `U256` cross-multiplication; fee threshold checked without division:

$$
|p_{\text{dex}} - p_{\text{cex}}| \cdot 10\,000 \;\le\; p_{\text{dex}} \cdot \text{fee\_bps}
\quad \Rightarrow \quad \text{SpreadBelowFee}
$$

---

## 5. CEX-DEX Profit (candidate sweep)

Unlike DEX-DEX (which uses a closed-form optimal input), CEX-DEX uses a
**candidate-sweep** search over exponential + linear grids:

1. **Exponential grid:** `max_input >> exponent` for $\text{exponent} \in [0, 24]$
2. **Linear grid:** `max_input × step / 40` for $\text{step} \in [1, 40]$

where `max_input = reserve / 10` (10% of the on-side reserve).

For each candidate input $x$:

| Direction | Leg 1 (DEX) | Leg 2 (CEX, hypothetical) | Profit |
|-----------|-------------|---------------------------|--------|
| **SellOnDex** | sell $x$ WETH → get $q$ quote | buy $x$ WETH on CEX costs $x \cdot p_{\text{cex}} / 10^{18}$ quote | $q - \text{cost}$ (in quote, then ÷ $p_{\text{cex}}$ → WETH) |
| **BuyOnDex** | buy WETH with $x$ quote → get $w$ WETH | sell $w$ WETH on CEX yields $w \cdot p_{\text{cex}} / 10^{18}$ quote | $\text{yield} - x$ (in quote, then ÷ $p_{\text{cex}}$ → WETH) |

Best profit across all candidates is returned.

**Implementation:** [`cex_dex_arb.rs` `estimate_profit_wei`](../crates/mev-sim/src/strategies/cex_dex_arb.rs) —
all arithmetic in `U256`.

---

## 6. f64 Boundary

`f64` appears **only** at intake and display layers:

| Function | Role | Used in decisions? |
|----------|------|--------------------|
| `cex_price_f64_to_fp` | Converts external `f64` price → `u128` fixed-point | No (intake only) |
| `cex_price_fp_to_f64` | Converts `u128` FP → `f64` for debug logging | No (display only) |
| `format_price_from_quote_per_weth` | Renders `U256` price as `f64` for tracing | No (display only) |

**No `f64` value participates in any spread comparison, fee check, or profit calculation.**

---

## 7. Data-Quality Verdicts (CEX-DEX)

The `CexDexVerdict` enum gates every evaluation:

| Variant | Meaning | Gate condition |
|---------|---------|----------------|
| `NoCexData` | No CEX price point available | `cex_price` is `None` |
| `StaleCexData` | CEX candle too far from block timestamp | `delta_seconds > configured_cex_stale_seconds()` |
| `SpreadBelowFee` | Spread exists but < fee floor | Integer cross-multiply check |
| `NonPositiveProfit` | Spread wide enough, but sweep yields 0 profit | After candidate sweep |
| `Opportunity` | Profitable trade detected | All gates passed |

`NoCexData` and `StaleCexData` are **data quality** dismissals.
`SpreadBelowFee` is an **economic** dismissal. They are disjoint code paths.
