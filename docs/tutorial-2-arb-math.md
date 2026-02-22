# Tutorial 2: Arbitrage Math from First Principles

This tutorial derives constant-product AMM arbitrage step-by-step and shows how it maps to code.

**Prerequisites**: Familiarity with DEX concepts (pools, reserves, swaps). Read [MEV Glossary § Arbitrage](mev-concepts-glossary.md#arbitrage) first if needed.

---

## Part 1: The Setup

### Two Pools, Same Tokens

Imagine two Uniswap V2 pools offering different prices for the same token pair:

**Pool A:**
- Token0 (USDC): 1,000,000 tokens, reserve0
- Token1 (WETH): 1,000 tokens, reserve1
- Price: 1000 USDC per WETH (1 WETH = 1000 USDC)

**Pool B:**
- Token0 (USDC): 2,000,000 tokens, reserve0
- Token1 (WETH): 1,050 tokens, reserve1
- Price: ~1904.76 USDC per WETH (2M / 1.05k = 1904.76)

**Price difference**: Pool A is cheaper (1000 USDC/WETH) than Pool B (1904.76 USDC/WETH)

### The Arbitrage

We want to:
1. Buy WETH from Pool A (cheaper)
2. Sell WETH to Pool B (expensive)
3. Keep the profit

---

## Part 2: Constant-Product Invariant

Uniswap V2 uses the constant-product formula:

$$x \cdot y = k$$

Where:
- $x$ = reserve0 (USDC)
- $y$ = reserve1 (WETH)  
- $k$ = constant (product stays the same for each pool)

### Pool A Invariant
$$k_A = 1{,}000{,}000 \times 1{,}000 = 1{,}000{,}000{,}000$$

### Pool B Invariant
$$k_B = 2{,}000{,}000 \times 1{,}050 = 2{,}100{,}000{,}000$$

---

## Part 3: The Swap Formula

When we swap into a pool, the invariant is maintained. After a swap:

$$(\text{reserve}_{\text{in}} + \text{amount\_in\_with\_fee}) \times (\text{reserve}_{\text{out}} - \text{amount\_out}) = k$$

Solving for $\text{amount\_out}$:

$$\text{amount\_out} = \frac{\text{amount\_in\_with\_fee} \times \text{reserve\_out}}{\text{reserve\_in} + \text{amount\_in\_with\_fee}}$$

Where:
$$\text{amount\_in\_with\_fee} = \text{amount\_in} \times 0.997$$

(Uniswap V2 charges 0.3% fee, so we keep 99.7% of input)

### Code Reference

This formula is implemented in [arbitrage.rs lines 147–160](../crates/mev-sim/src/strategies/arbitrage.rs#L147):

```rust
fn amount_out(amount_in: u128, reserve_in: u128, reserve_out: u128) -> u128 {
    let amount_in_with_fee = amount_in.saturating_mul(997);  // 99.7%
    let numerator = amount_in_with_fee.saturating_mul(reserve_out);
    let denominator = reserve_in
        .saturating_mul(1000)
        .saturating_add(amount_in_with_fee);
    numerator / denominator
}
```

**Note**: Multiplies by 1000 instead of dividing by 1000 to preserve precision (fixed-point math).

---

## Part 4: Two-Leg Arbitrage

We execute two swaps:

### Leg 1: Swap into Pool A
- Send: `X` USDC to Pool A
- Receive: `Y` WETH from Pool A

Using the swap formula:
$$Y = \frac{X \times 0.997 \times 1{,}000}{1{,}000{,}000 + X \times 0.997}$$

### Leg 2: Swap into Pool B
- Send: `Y` WETH to Pool B
- Receive: `Z` USDC from Pool B

Using the swap formula again:
$$Z = \frac{Y \times 0.997 \times 2{,}000{,}000}{1{,}050 + Y \times 0.997}$$

### Profit
$$\text{profit} = Z - X$$

(We get back `Z` USDC but spent `X` USDC, so profit is `Z - X`)

### Code Reference

Two-leg evaluation in [arbitrage.rs lines 230–241](../crates/mev-sim/src/strategies/arbitrage.rs#L230):

```rust
fn estimate_profit(input: u128, pool_1: &PoolState, pool_2: &PoolState) -> u128 {
    let leg_1_out = amount_out(input, pool_1.reserve0, pool_1.reserve1);  // Leg 1
    if leg_1_out == 0 {
        return 0;
    }
    let leg_2_out = amount_out(leg_1_out, pool_2.reserve1, pool_2.reserve0);  // Leg 2
    leg_2_out.saturating_sub(input)  // Profit = output - input
}
```

---

## Part 5: Optimal Input Derivation

### The Problem

Given two pools, what input `X` maximizes profit?

We want to find:
$$\max_X \left( Z(X) - X \right)$$

Where:
$$Z(X) = \frac{\left(\frac{Y(X) \times 0.997 \times r_{b,0}}{r_{b,1} + Y(X) \times 0.997}\right) \times 0.997 \times r_{a,0}}{r_{a,0} + r_{a,0}}$$

(Ugh! Nested and messy.)

### The Solution

In the limit where fees are negligible and reserves are large, the optimal input is:

$$X^* = \sqrt{\frac{r_{a,0} \times r_{b,0} \times r_{a,1}}{r_{b,1}}} - r_{a,0}$$

Where:
- $r_{a,0}, r_{a,1}$ = reserves in pool A (buy pool)
- $r_{b,0}, r_{b,1}$ = reserves in pool B (sell pool)

**Intuition**: At the optimal point, the price in pool A (after swap) matches the price in pool B (after swap).

### Code Reference

Implemented in [arbitrage.rs lines 189–209](../crates/mev-sim/src/strategies/arbitrage.rs#L189):

```rust
fn optimal_input_from_spec(pool_buy: &PoolState, pool_sell: &PoolState) -> u128 {
    // optimal_in = sqrt(r0_a * r0_b * r1_a / r1_b) - r0_a
    let inside =
        (pool_buy.reserve0 as f64) * (pool_sell.reserve0 as f64) * (pool_buy.reserve1 as f64)
            / (pool_sell.reserve1 as f64);
    
    let optimal = inside.sqrt() - (pool_buy.reserve0 as f64);
    optimal as u128
}
```

---

## Part 6: Example Calculation

### Inputs

Pool A (buy):
- $r_{a,0} = 1{,}000{,}000$ USDC
- $r_{a,1} = 1{,}000$ WETH

Pool B (sell):
- $r_{b,0} = 2{,}000{,}000$ USDC
- $r_{b,1} = 1{,}050$ WETH

### Step 1: Price Discrepancy Check

Price in Pool A: $p_A = r_{a,1} / r_{a,0} = 1{,}000 / 1{,}000{,}000 = 0.001$  
Price in Pool B: $p_B = r_{b,1} / r_{b,0} = 1{,}050 / 2{,}000{,}000 = 0.000525$

Discrepancy:
$$\text{discrepancy} = \frac{|p_A - p_B|}{\min(p_A, p_B)} = \frac{0.000475}{0.000525} \approx 0.905 = 90.5\%$$

This is **huge** (> 0.1% threshold), so we proceed.

### Step 2: Optimal Input

$$X^* = \sqrt{\frac{1{,}000{,}000 \times 2{,}000{,}000 \times 1{,}000}{1{,}050}} - 1{,}000{,}000$$

$$X^* = \sqrt{\frac{2{,}000{,}000{,}000{,}000{,}000}{1{,}050}} - 1{,}000{,}000$$

$$X^* = \sqrt{1{,}904{,}761{,}904{,}761} - 1{,}000{,}000$$

$$X^* = 1{,}380{,}340 - 1{,}000{,}000 = 380{,}340 \text{ USDC}$$

### Step 3: Leg 1 Output

Send 380,340 USDC to Pool A, receive WETH:

$$Y = \frac{380{,}340 \times 0.997 \times 1{,}000}{1{,}000{,}000 + 380{,}340 \times 0.997}$$

$$Y = \frac{379{,}058{,}580}{1{,}379{,}058} \approx 275$$ WETH

### Step 4: Leg 2 Output

Send 275 WETH to Pool B, receive USDC:

$$Z = \frac{275 \times 0.997 \times 2{,}000{,}000}{1{,}050 + 275 \times 0.997}$$

$$Z = \frac{548{,}350{,}000}{1{,}323} \approx 414{,}700$$ USDC

### Step 5: Profit

$$\text{Profit} = Z - X = 414{,}700 - 380{,}340 = 34{,}360 \text{ USDC}$$

**Profit after fees: 34,360 USDC (~2.9% return on capital!)**

---

## Part 7: Threshold Filtering

We don't pursue every opportunity. The code has two filters:

### Filter 1: Price Discrepancy Threshold

Code [arbitrage.rs lines 251–255](../crates/mev-sim/src/strategies/arbitrage.rs#L251):

```rust
// Threshold: discrepancy > 0.1% = 10 bps.
let discrepancy_bps = relative_discrepancy_bps(pool_a, pool_b);
if discrepancy_bps <= 10 {
    return None;
}
```

**Why?** Small discrepancies are either:
- Flash loan bots beating us to it
- Too small to cover gas fees

### Filter 2: Gas Floor

Code [arbitrage.rs lines 257–259](../crates/mev-sim/src/strategies/arbitrage.rs#L257):

```rust
let gas_floor = 200_000u128.saturating_mul(base_fee);
if profit_estimate_wei < gas_floor {
    return None;
}
```

**Why?** The arbitrage costs gas to execute.

Example: If base_fee = 25 gwei = 25 × 10^9 Wei:
- Gas cost: 200,000 × 25 × 10^9 = 5 × 10^15 Wei = 0.005 ETH

Profit must exceed this or it's not worth the gas.

---

## Part 8: Direction Selection

Since price differences can go either way, we check both directions:

Code [arbitrage.rs lines 260–271](../crates/mev-sim/src/strategies/arbitrage.rs#L260):

```rust
// Evaluate both directions and keep the best profitable one.
let input_ab = optimal_input_from_spec(pool_a, pool_b);
let profit_ab = estimate_profit(input_ab, pool_a, pool_b);

let input_ba = optimal_input_from_spec(pool_b, pool_a);
let profit_ba = estimate_profit(input_ba, pool_b, pool_a);

// Pick the direction with highest profit
let (pool_1, pool_2, optimal_input_wei, profit_estimate_wei) = if profit_ab > profit_ba {
    (pool_a.address, pool_b.address, input_ab, profit_ab)
} else if profit_ba > profit_ab {
    (pool_b.address, pool_a.address, input_ba, profit_ba)
} else if input_ab >= input_ba {
    (pool_a.address, pool_b.address, input_ab, profit_ab)
} else {
    (pool_b.address, pool_a.address, input_ba, profit_ba)
};
```

**Logic:**
1. If A→B is more profitable, use that route
2. Else if B→A is more profitable, use that route
3. Else (both same profit), pick the one with larger input (larger opportunity)

---

## Part 9: Full Detection Function

The complete flow, with all filters, in [arbitrage.rs lines 243–289](../crates/mev-sim/src/strategies/arbitrage.rs#L243):

```rust
pub fn detect_v2_arb_opportunity(
    pool_a: &PoolState,
    pool_b: &PoolState,
    base_fee: u128,
) -> Option<ArbOpportunity> {
    // 1. Check compatible tokens
    if pool_a.token0 != pool_b.token0 || pool_a.token1 != pool_b.token1 {
        return None;
    }

    // 2. Price discrepancy filter (> 10 bps)
    let discrepancy_bps = relative_discrepancy_bps(pool_a, pool_b);
    if discrepancy_bps <= 10 {
        return None;
    }

    let gas_floor = 200_000u128.saturating_mul(base_fee);

    // 3. Evaluate both directions
    let input_ab = optimal_input_from_spec(pool_a, pool_b);
    let profit_ab = estimate_profit(input_ab, pool_a, pool_b);

    let input_ba = optimal_input_from_spec(pool_b, pool_a);
    let profit_ba = estimate_profit(input_ba, pool_b, pool_a);

    // 4. Pick best direction
    let (pool_1, pool_2, optimal_input_wei, profit_estimate_wei) = if profit_ab > profit_ba {
        (pool_a.address, pool_b.address, input_ab, profit_ab)
    } else if profit_ba > profit_ab {
        (pool_b.address, pool_a.address, input_ba, profit_ba)
    } else if input_ab >= input_ba {
        (pool_a.address, pool_b.address, input_ab, profit_ab)
    } else {
        (pool_b.address, pool_a.address, input_ba, profit_ba)
    };

    // 5. Gas floor filter
    if optimal_input_wei == 0 || profit_estimate_wei < gas_floor {
        return None;
    }

    Some(ArbOpportunity {
        token_a: pool_a.token0,
        token_b: pool_a.token1,
        pool_1,
        pool_2,
        profit_estimate_wei,
        optimal_input_wei,
        trade_path: vec![pool_a.token0, pool_a.token1, pool_a.token0],
    })
}
```

---

## Part 10: Real-World Complications

### 1. Multi-Pool Arbitrage

Our formula handles 2-pool cycles. Real MEV bots scan:
- **3-way swaps**: USDC → WETH → USDT → USDC
- **4+ pools** (complex routing)

Extension: Apply the same math recursively across each leg.

### 2. State Changes

When multiple bots detect the same opportunity:
1. First bot wins
2. Prices move after their swap
3. Second bot now faces much worse prices
4. "Cascading losses"

Our code assumes **static state**. In live trading, update state between swaps (see `EvmFork::simulate_tx`).

### 3. Slippage & Price Impact

Our formula assumes reserves stay roughly constant. In reality:
- Swapping changes reserves
- Slippage kills profit
- Each MEV bot's swap pushes prices against the next

**Mitigation**: Use smaller inputs or accept lower profit margins.

### 4. Gas Costs & MEV Auctions

Real searchers pay:
- 200,000 gas × base_fee (simple execution)
- But also **priority fee** to get included quickly
- Bundle bribes to builders
- MEV auction pressure reduces profit

Our code captures this via `gas_floor`.

---

## Part 11: Extending the Code

Want to add a new detection strategy?

1. **Define the math** (derive formulas)
2. **Implement helper functions** `fn helper(...)` 
3. **Add to strategies/mod.rs** and export
4. **Test with unit tests** (copy pattern from lines 400+)
5. **Integrate into CLI** (add command variant)

Example: See **Contributing** in [README.md](../README.md#contributing).

---

## Key Takeaways

✅ **Arbitrage = buy low, sell high** across two pools  
✅ **Constant-product formula** ensures market-clearing prices  
✅ **Optimal input** maximizes profit (calculus magic ✨)  
✅ **Thresholds filter** uneconomical opportunities  
✅ **Math is exact** in code; real MEV has slippage + gas costs  
✅ **Direction matters** — check both ways!

Happy arbitraging! 🤖

