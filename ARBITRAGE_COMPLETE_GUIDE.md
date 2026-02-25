# Complete MEV Arbitrage Detection: Corrected Guide

**Status:** Updated with post-audit corrections (code + docs aligned)

## What was corrected

- `isqrt` now returns floor for oscillating Newton cases by returning `y`, not `x`.
- Closed-form neighborhood verification now scans the full `[x-16, x+16]` range (33 points), not only 3 points.
- Equal-profit direction tie-break now actually prefers smaller input size.
- Gas is converted from ETH wei to token0 wei using a same-block WETH reference pool when token0 is not WETH.
- If conversion is required and no reference pool is provided, detection returns `Err(ArbError::MissingReferencePrice)`.
- Misleading comments were corrected (10 bps is a loose prefilter, not fee coverage).
- Closed-form now guards reserve domain (`uint112` ceiling) and uses checked U256 arithmetic to avoid overflow in presqrt path.

## Core function behavior

### `detect_v2_arb_opportunity`

Signature:

```rust
pub fn detect_v2_arb_opportunity(
    pool_a: &PoolState,
    pool_b: &PoolState,
    base_fee: u128,
    weth_price_pool: Option<&PoolState>,
) -> Result<Option<ArbOpportunity>, ArbError>
```

Semantics:

- `Ok(Some(opportunity))`: profitable after gas conversion.
- `Ok(None)`: valid rejection (no discrepancy, no profitable direction, or non-positive net).
- `Err(ArbError::...)`: fault (state mismatch, missing reference price, etc.).

### Gas conversion policy

- Gas estimate: `200_000 * base_fee` (ETH wei).
- If `token0 == WETH`, gas is already in route units.
- Else, conversion uses `weth_price_pool` at same block:
  - pool `(WETH, token0)`: `amount_out(gas_eth, reserve_weth, reserve_token0, fee)`
  - pool `(token0, WETH)`: `amount_out(gas_eth, reserve_weth_side, reserve_token0_side, fee)`
- Missing/incompatible reference returns `ArbError::MissingReferencePrice`.

## Math notes (corrected)

### AMM swap output

For fee-on-input constant product pools:

$$
\text{amount\_out} = \left\lfloor \frac{\text{amount\_in}\cdot f_n\cdot r_{out}}{r_{in}\cdot f_d + \text{amount\_in}\cdot f_n} \right\rfloor
$$

Rounding down is standard integer AMM behavior; it does **not** prevent sandwich attacks.

### Discrepancy prefilter

Implemented by exact rational comparison:

$$
\frac{|p_a-p_b|}{\min(p_a,p_b)} > \frac{\text{threshold\_bps}}{10000}
$$

with cross multiplication to avoid floats.

The `10 bps` value is a **noise prefilter** only; it is not a guarantee of post-fee profitability.

### Integer square root

`isqrt` uses Newton iterations and returns `y` on exit, which is the last non-increasing iterate and equals $\lfloor\sqrt{n}\rfloor$ even on oscillating inputs (e.g., `n=3`, `n=8`).

### Closed-form sizing

- Used only for matching fee structures.
- Includes feasibility check (`sqrt_presqrt >= fee_denominator`).
- Falls back to ternary search when infeasible or out of reserve domain.
- Neighborhood refinement scans all points in `[x-16, x+16]`.

## Fee terminology correction

- Uniswap V2 swap fee is `0.3%` (`997/1000`).
- SushiSwap V2 trader-facing swap fee is also `0.3%` (`997/1000`), even though internal distribution differs.
- A `0.25%` fee example should be described as a custom V2 fork / non-standard venue example, not Sushi V2.

## Tests added/updated

- Added `isqrt_returns_floor_on_oscillating_inputs` with cases:
  - `isqrt(3) == 1`
  - `isqrt(8) == 2`
  - plus boundary sanity checks.
- Added integration test requiring reference pool for non-WETH gas conversion.
- Existing arbitrage and simulation tests remain passing.

## Practical constraints

- Same-block checks reduce stale-state errors but are not a full execution guarantee.
- Ternary search remains a heuristic on a discrete landscape; neighborhood scan and fallback behavior reduce, but do not eliminate, rounding artifacts.
- `max_input = 10% reserve` is a risk cap heuristic, not a mathematical optimum bound.
