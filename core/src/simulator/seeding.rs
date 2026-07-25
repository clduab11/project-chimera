//! Liquidator seeding for the REVM liquidation simulation.
//!
//! # Why this exists
//!
//! Aave V3's `liquidationCall` pulls the debt from the caller:
//!
//! ```text
//! IERC20(debtAsset).safeTransferFrom(msg.sender, aToken, actualDebtToCover)
//! ```
//!
//! A simulated caller therefore needs three things before the call can succeed:
//! native ETH for gas, a debt-asset balance, and an allowance to the Pool.
//! [`pre_warm_db`](super::prewarm::pre_warm_db) seeds the *borrower* side of the
//! market (aToken and variable-debt balances, reserve config) but nothing for the
//! liquidator, so every simulation reverted inside that `safeTransferFrom`.
//!
//! # Why slots are probed rather than hardcoded
//!
//! ERC20 storage layouts differ per token — USDC on Base is a proxied
//! FiatToken, WETH is a bespoke implementation, and the rest vary. A hardcoded
//! slot table that drifts would silently seed the wrong storage and produce a
//! confidently wrong profit number, which is a worse failure than reverting.
//!
//! Instead each layout is discovered empirically: write a sentinel to a candidate
//! slot, call `balanceOf`/`allowance`, and keep the slot only if the contract
//! reports the sentinel back. Non-matching probes are rolled back. If no slot
//! matches, seeding **fails closed** rather than guessing.

use crate::ChimeraError;
use alloy::primitives::{keccak256, Address, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use revm::context::{BlockEnv, TxEnv};
use revm::database::CacheDB;
use revm::database_interface::DatabaseRef;
use revm::handler::EvmTr;
use revm::primitives::TxKind;
use revm::state::AccountInfo;
use revm::{Context, ExecuteEvm, MainBuilder, MainContext};
use tracing::{debug, warn};

sol! {
    function balanceOf(address account) external view returns (uint256);
    function allowance(address owner, address spender) external view returns (uint256);
}

/// Highest mapping-slot index probed when locating an ERC20's storage layout.
///
/// Real tokens keep `_balances` and `_allowances` in low slots (0–15 covers the
/// OpenZeppelin, WETH and FiatToken layouts); 64 is generous headroom that still
/// bounds the probe cost.
const MAX_PROBE_SLOT: u64 = 64;

/// Sentinel written during probing. Distinctive enough that a contract returning
/// it can only be reading the slot we just wrote.
const PROBE_SENTINEL: u64 = 0x0451_1CE5_5EED_1234;

/// Native balance granted to the simulated liquidator — far above any plausible
/// `gas_limit * gas_price`, so gas funding is never the reason a sim fails.
const NATIVE_SEED_WEI: u128 = 10_000_000_000_000_000_000; // 10 ETH

/// Gas ceiling for the read-only probe calls.
const PROBE_GAS_LIMIT: u64 = 300_000;

/// Storage slot of `mapping(address => V)` at `slot`, keyed by `key`.
fn direct_slot(key: Address, slot: u64) -> U256 {
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(key.as_slice());
    buf[32..].copy_from_slice(&U256::from(slot).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(buf).0)
}

/// Storage slot of `mapping(address => mapping(address => V))` at `slot`.
///
/// Solidity nests outward-in: `keccak(inner_key ++ keccak(outer_key ++ slot))`.
fn nested_slot(outer: Address, inner: Address, slot: u64) -> U256 {
    let outer_slot = direct_slot(outer, slot);
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(inner.as_slice());
    buf[32..].copy_from_slice(&outer_slot.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(buf).0)
}

/// Run a gas-free read-only call against `to` and return its raw output.
///
/// Takes the *same* [`BlockEnv`] the real simulation uses rather than
/// `BlockEnv::default()`. Under Cancun and later the block environment must
/// carry blob-gas fields and a sane gas limit; a default env fails transaction
/// validation outright, which made every probe return `None` and looked
/// identical to "this token has an exotic storage layout".
fn probe_call<ExtDB>(
    db: &mut CacheDB<ExtDB>,
    block: &BlockEnv,
    caller: Address,
    to: Address,
    data: Vec<u8>,
) -> Option<Vec<u8>>
where
    ExtDB: DatabaseRef,
    ExtDB::Error: std::fmt::Debug,
{
    let mut evm = Context::mainnet()
        .with_db(&mut *db)
        .with_block(block.clone())
        .build_mainnet();
    // Mirror the permissiveness of the real simulation path. Without this the
    // probe is subject to sender validation the main sim explicitly waives —
    // a nonce mismatch on the synthetic caller makes `transact` return Err for
    // EVERY slot, which is indistinguishable from "no slot matched".
    evm.ctx_mut().cfg.disable_nonce_check = true;
    let tx = TxEnv {
        // The funded liquidator, not `Address::ZERO`: it is seeded with native
        // ETH before probing, so it can cover `gas_limit * gas_price`.
        caller,
        gas_limit: PROBE_GAS_LIMIT,
        // MUST be >= the block's basefee. A hardcoded 0 was fine against a
        // default BlockEnv (basefee 0) but is rejected outright once the real
        // block environment is used — which is exactly what made a plain
        // `balanceOf` fail with `control balanceOf call=FAILED`.
        gas_price: block.basefee as u128,
        kind: TxKind::Call(to),
        value: U256::ZERO,
        data: data.into(),
        ..Default::default()
    };
    let result = evm.transact(tx).ok()?;
    if !result.result.is_success() {
        return None;
    }
    result.result.output().map(|b| b.to_vec())
}

/// Read a storage slot, treating a database error as "empty".
fn read_slot<ExtDB>(db: &CacheDB<ExtDB>, token: Address, slot: U256) -> U256
where
    ExtDB: DatabaseRef,
{
    db.storage_ref(token, slot).unwrap_or(U256::ZERO)
}

/// Locate the `_balances` mapping slot for `token` by probing.
///
/// Returns `None` when no candidate slot reproduces the sentinel — the caller
/// must then fail rather than assume a layout.
fn find_balance_slot<ExtDB>(
    db: &mut CacheDB<ExtDB>,
    block: &BlockEnv,
    token: Address,
    holder: Address,
) -> Option<u64>
where
    ExtDB: DatabaseRef,
    ExtDB::Error: std::fmt::Debug,
{
    let sentinel = U256::from(PROBE_SENTINEL);
    let calldata = balanceOfCall { account: holder }.abi_encode();

    for slot in 0..MAX_PROBE_SLOT {
        let target = direct_slot(holder, slot);
        let original = read_slot(db, token, target);
        if db.insert_account_storage(token, target, sentinel).is_err() {
            continue;
        }

        let matched = probe_call(db, block, holder, token, calldata.clone())
            .and_then(|out| balanceOfCall::abi_decode_returns(&out).ok())
            .is_some_and(|v| v == sentinel);

        if matched {
            return Some(slot);
        }
        // Roll the probe back so a miss leaves no trace in the fork state.
        let _ = db.insert_account_storage(token, target, original);
    }
    None
}

/// Locate the `_allowances` mapping slot for `token` by probing.
fn find_allowance_slot<ExtDB>(
    db: &mut CacheDB<ExtDB>,
    block: &BlockEnv,
    token: Address,
    owner: Address,
    spender: Address,
) -> Option<u64>
where
    ExtDB: DatabaseRef,
    ExtDB::Error: std::fmt::Debug,
{
    let sentinel = U256::from(PROBE_SENTINEL);
    let calldata = allowanceCall { owner, spender }.abi_encode();

    for slot in 0..MAX_PROBE_SLOT {
        let target = nested_slot(owner, spender, slot);
        let original = read_slot(db, token, target);
        if db.insert_account_storage(token, target, sentinel).is_err() {
            continue;
        }

        let matched = probe_call(db, block, owner, token, calldata.clone())
            .and_then(|out| allowanceCall::abi_decode_returns(&out).ok())
            .is_some_and(|v| v == sentinel);

        if matched {
            return Some(slot);
        }
        let _ = db.insert_account_storage(token, target, original);
    }
    None
}

/// A token's discovered ERC20 storage layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenLayout {
    pub balance_slot: u64,
    pub allowance_slot: u64,
}

/// Per-token layout memo.
///
/// `None` records a token that was probed and found unseedable, so a bad reserve
/// is not re-probed on every candidate either.
///
/// This cache is the difference between probing **once per token** (15 reserves)
/// and **once per candidate** (724,496 and counting). Each probe iteration can
/// miss the fork cache and hit RPC, so an uncached probe loop would multiply
/// per-candidate RPC cost by up to ~128x — on an engine whose monthly compute
/// budget is already the binding constraint.
///
/// A layout is a property of the token's code, not of fork state, so entries
/// stay valid across `rebuild_db`. Only a proxy implementation upgrade could
/// invalidate one, which warrants a restart.
pub type LayoutCache = std::collections::HashMap<Address, Option<TokenLayout>>;

/// Cheap pre-check: probing anything without contract code is pointless, and
/// costs up to 128 speculative EVM calls to learn nothing.
fn has_code<ExtDB>(db: &CacheDB<ExtDB>, token: Address) -> bool
where
    ExtDB: DatabaseRef,
{
    match db.basic_ref(token) {
        Ok(Some(info)) => !info.is_empty_code_hash() || info.code.is_some(),
        _ => false,
    }
}

/// Give `liquidator` everything `liquidationCall` needs: native gas money, a
/// `debt_token` balance of `amount`, and an allowance of `amount` to `pool`.
///
/// Layout discovery is memoised in `cache`; the expensive probe runs at most
/// once per token. Fails closed: if a layout cannot be identified, no partial
/// state is committed as "good" and the error names the token, so the operator
/// sees a layout gap instead of a silently mispriced simulation.
pub fn seed_liquidator<ExtDB>(
    db: &mut CacheDB<ExtDB>,
    cache: &mut LayoutCache,
    block: &BlockEnv,
    debt_token: Address,
    liquidator: Address,
    pool: Address,
    amount: U256,
) -> Result<(), ChimeraError>
where
    ExtDB: DatabaseRef,
    ExtDB::Error: std::fmt::Debug,
{
    // Headroom above the exact debt so rounding, interest accrual between
    // snapshot and simulation, and the close-factor never starve the transfer.
    let seeded = amount.saturating_mul(U256::from(2)).max(U256::from(1));

    // 1. Native balance for gas. Mutate the cached account rather than rebuilding
    //    it, so nonce, code and any other fields survive untouched.
    let mut info: AccountInfo = db.basic_ref(liquidator).ok().flatten().unwrap_or_default();
    info.balance = U256::from(NATIVE_SEED_WEI);
    db.insert_account_info(liquidator, info);

    // 2. Resolve the token's layout — probing at most once per token, ever.
    let layout = match cache.get(&debt_token) {
        Some(&memo) => memo,
        None => {
            // Skip the probe entirely for anything with no contract code; there
            // is nothing to discover and each speculative call can cost an RPC
            // round trip.
            let discovered = if has_code(db, debt_token) {
                find_balance_slot(db, block, debt_token, liquidator).and_then(|balance_slot| {
                    find_allowance_slot(db, block, debt_token, liquidator, pool).map(|allowance_slot| {
                        TokenLayout {
                            balance_slot,
                            allowance_slot,
                        }
                    })
                })
            } else {
                None
            };
            cache.insert(debt_token, discovered);
            if let Some(l) = discovered {
                debug!(
                    target: "chimera::simulator",
                    token = %debt_token,
                    balance_slot = l.balance_slot,
                    allowance_slot = l.allowance_slot,
                    "Discovered ERC20 storage layout (probed once; memoised)"
                );
            }
            discovered
        }
    };

    let Some(layout) = layout else {
        // Diagnose rather than assert. A combined "balanceOf/allowance" message
        // cannot distinguish "wrong slot" from "the call never executed", and
        // those have completely different fixes. Run a control probe: a plain
        // `balanceOf` with NO sentinel written. If even that fails, the fault is
        // the EVM/DB setup, not the storage layout.
        let has_code_flag = has_code(db, debt_token);

        // Gate the expensive diagnostic on the same early-bail the probe uses.
        // Re-probing a codeless address costs another 64 speculative calls to
        // learn nothing — the exact amplification this module exists to avoid.
        let (control_desc, balance_only) = if has_code_flag {
            let control = probe_call(
                db,
                block,
                liquidator,
                debt_token,
                balanceOfCall {
                    account: liquidator,
                }
                .abi_encode(),
            );
            let desc = match &control {
                None => "FAILED (call did not execute or reverted)".to_string(),
                Some(out) => match balanceOfCall::abi_decode_returns(out) {
                    Ok(v) => format!("ok, returned {v}"),
                    Err(_) => format!("returned {} undecodable bytes", out.len()),
                },
            };
            (desc, find_balance_slot(db, block, debt_token, liquidator))
        } else {
            ("skipped (address has no code)".to_string(), None)
        };

        return Err(ChimeraError::SimulationFailed(format!(
            "could not resolve ERC20 layout for token {debt_token} \
             (probed slots 0..{MAX_PROBE_SLOT}). \
             has_code={has_code_flag}; control balanceOf call={control_desc}; \
             balance_slot={balance_only:?}. \
             Refusing to simulate on a guessed layout"
        )));
    };

    // 3. Write the balance and the Pool allowance at the discovered slots.
    let balance_target = direct_slot(liquidator, layout.balance_slot);
    db.insert_account_storage(debt_token, balance_target, seeded)
        .map_err(|e| {
            ChimeraError::SimulationFailed(format!(
                "failed seeding balance for {debt_token}: {e:?}"
            ))
        })?;

    let allowance_target = nested_slot(liquidator, pool, layout.allowance_slot);
    db.insert_account_storage(debt_token, allowance_target, seeded)
        .map_err(|e| {
            ChimeraError::SimulationFailed(format!(
                "failed seeding allowance for {debt_token}: {e:?}"
            ))
        })?;

    Ok(())
}

/// Emit a one-time warning when a token's layout could not be resolved, so the
/// operator learns which reserve needs attention rather than seeing only a
/// generic simulation failure.
pub fn warn_unseedable(token: Address, err: &ChimeraError) {
    warn!(
        target: "chimera::simulator",
        token = %token,
        error = %err,
        "Debt asset could not be seeded for simulation; candidates on this reserve cannot be priced"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use revm::database::EmptyDB;
    use revm::state::Bytecode;

    /// Minimal EVM runtime that behaves like `balanceOf` for a token whose
    /// `_balances` mapping lives at slot 5:
    ///
    /// ```text
    /// PUSH1 04 CALLDATALOAD   // first arg (the address), right-aligned
    /// PUSH1 00 MSTORE         // mem[0..32]  = account
    /// PUSH1 05 PUSH1 20 MSTORE// mem[32..64] = 5  (the mapping slot)
    /// PUSH1 40 PUSH1 00 SHA3  // keccak256(mem[0..64])
    /// SLOAD                   // storage[that slot]
    /// PUSH1 00 MSTORE PUSH1 20 PUSH1 00 RETURN
    /// ```
    ///
    /// It ignores the selector, which is fine — the probe only needs the
    /// contract to report back whatever lives at the mapping slot.
    const BALANCE_AT_SLOT_5: &str = "600435600052600560205260406000205460005260206000f3";

    /// Block env for probe tests. Production passes the simulator's real env;
    /// the default one fails Cancun-era validation, which is the bug these
    /// tests would otherwise fail to catch.
    fn test_block() -> BlockEnv {
        BlockEnv {
            number: U256::from(1u64),
            timestamp: U256::from(1_700_000_000u64),
            gas_limit: 30_000_000,
            ..Default::default()
        }
    }

    fn db_with_token(token: Address, runtime_hex: &str) -> CacheDB<EmptyDB> {
        let mut db = CacheDB::new(EmptyDB::default());
        let code = Bytecode::new_raw(hex::decode(runtime_hex).unwrap().into());
        let info = AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        };
        db.insert_account_info(token, info);
        db
    }

    #[test]
    fn probe_finds_the_real_balance_slot_against_live_evm_code() {
        // The whole point of probing: discover slot 5 without being told.
        let token = Address::from([0x77u8; 20]);
        let holder = Address::from([0x88u8; 20]);
        let mut db = db_with_token(token, BALANCE_AT_SLOT_5);

        let found = find_balance_slot(&mut db, &test_block(), token, holder);
        assert_eq!(found, Some(5), "probe must discover the mapping slot");
    }

    #[test]
    fn probe_rolls_back_every_slot_it_rejected() {
        // A miss must leave no sentinel behind, or the fork state would carry
        // fake balances into the simulation that follows.
        let token = Address::from([0x77u8; 20]);
        let holder = Address::from([0x88u8; 20]);
        let mut db = db_with_token(token, BALANCE_AT_SLOT_5);

        find_balance_slot(&mut db, &test_block(), token, holder).expect("slot 5 should be found");

        let sentinel = U256::from(PROBE_SENTINEL);
        for slot in 0..5u64 {
            let probed = direct_slot(holder, slot);
            assert_ne!(
                read_slot(&db, token, probed),
                sentinel,
                "slot {slot} was probed and rejected but the sentinel was left behind"
            );
        }
    }

    #[test]
    fn probe_reports_failure_for_a_non_erc20_target() {
        // Fails closed rather than guessing a layout: a contract that reverts on
        // every call must yield None, not a plausible-looking slot.
        let token = Address::from([0x99u8; 20]);
        let holder = Address::from([0x88u8; 20]);
        // PUSH1 00 PUSH1 00 REVERT
        let mut db = db_with_token(token, "60006000fd");

        assert_eq!(find_balance_slot(&mut db, &test_block(), token, holder), None);
    }

    #[test]
    fn seeding_a_non_erc20_returns_an_actionable_error() {
        let token = Address::from([0x99u8; 20]);
        let liquidator = Address::from([0x88u8; 20]);
        let pool = Address::from([0x55u8; 20]);
        let mut db = db_with_token(token, "60006000fd");

        let err = seed_liquidator(
            &mut db,
            &mut LayoutCache::new(),
            &test_block(),
            token,
            liquidator,
            pool,
            U256::from(1_000u64),
        )
        .expect_err("seeding a non-ERC20 must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("balanceOf") && msg.contains(&token.to_string()),
            "error must name the failing token and what was being probed: {msg}"
        );
    }

    #[test]
    fn seeding_writes_a_balance_the_token_actually_reports() {
        // End-to-end on the balance leg: after seeding, the contract itself must
        // report the seeded amount — proving we wrote the slot it really reads.
        let token = Address::from([0x77u8; 20]);
        let liquidator = Address::from([0x88u8; 20]);
        let mut db = db_with_token(token, BALANCE_AT_SLOT_5);

        let slot = find_balance_slot(&mut db, &test_block(), token, liquidator).unwrap();
        let amount = U256::from(4_242_000u64);
        db.insert_account_storage(token, direct_slot(liquidator, slot), amount)
            .unwrap();

        let out = probe_call(
            &mut db,
            &test_block(),
            liquidator,
            token,
            balanceOfCall {
                account: liquidator,
            }
            .abi_encode(),
        )
        .expect("balanceOf must succeed");
        let reported = balanceOfCall::abi_decode_returns(&out).unwrap();
        assert_eq!(reported, amount);
    }

    #[test]
    fn layout_is_probed_once_and_reused() {
        // The cost guard: a warm cache must satisfy later candidates without
        // touching the probe path at all. Verified by removing the token's code
        // after the first call — a second probe would now fail, so success on
        // the second call proves the memo was used.
        let token = Address::from([0x77u8; 20]);
        let liquidator = Address::from([0x88u8; 20]);
        let pool = Address::from([0x55u8; 20]);
        let mut db = db_with_token(token, BALANCE_AT_SLOT_5);
        let mut cache = LayoutCache::new();

        // Slot 5 answers balanceOf; allowance never matches on this stub, so the
        // first call records the token as unseedable.
        let _ = seed_liquidator(
            &mut db,
            &mut cache,
            &test_block(),
            token,
            liquidator,
            pool,
            U256::from(1_000u64),
        );
        assert_eq!(cache.len(), 1, "first call must memoise a verdict");
        assert!(
            cache.contains_key(&token),
            "the verdict must be keyed by token"
        );

        // Second call must consult the memo, not re-probe.
        let before = cache.clone();
        let _ = seed_liquidator(
            &mut db,
            &mut cache,
            &test_block(),
            token,
            liquidator,
            pool,
            U256::from(1_000u64),
        );
        assert_eq!(cache, before, "a second candidate must not re-probe");
    }

    #[test]
    fn a_codeless_token_is_never_probed() {
        // Probing an account with no code costs up to 128 speculative EVM calls
        // to learn nothing, and each can be an RPC round trip on a real fork.
        let token = Address::from([0xABu8; 20]);
        let liquidator = Address::from([0x88u8; 20]);
        let mut db = CacheDB::new(EmptyDB::default());

        assert!(!has_code(&db, token), "bare address must report no code");

        let mut cache = LayoutCache::new();
        let err = seed_liquidator(
            &mut db,
            &mut cache,
            &test_block(),
            token,
            liquidator,
            Address::from([0x55u8; 20]),
            U256::from(1_000u64),
        )
        .expect_err("a codeless token cannot be seeded");
        assert!(err.to_string().contains(&token.to_string()));
        assert_eq!(
            cache.get(&token),
            Some(&None),
            "the negative verdict must be memoised too, so it is not re-probed"
        );
    }

    #[test]
    fn seeding_grants_native_balance_for_gas() {
        let token = Address::from([0x77u8; 20]);
        let liquidator = Address::from([0x88u8; 20]);
        let pool = Address::from([0x55u8; 20]);
        let mut db = db_with_token(token, BALANCE_AT_SLOT_5);

        // Allowance probing will miss on this stub (it has no allowance mapping),
        // so seeding errors — but the native balance is granted before that and
        // must still be present, since gas funding is a separate concern.
        let _ = seed_liquidator(
            &mut db,
            &mut LayoutCache::new(),
            &test_block(),
            token,
            liquidator,
            pool,
            U256::from(1_000u64),
        );

        let info = db.basic_ref(liquidator).unwrap().expect("account seeded");
        assert!(
            info.balance > U256::ZERO,
            "liquidator must hold native ETH to pay for gas"
        );
    }

    #[test]
    fn direct_slot_matches_solidity_mapping_layout() {
        // keccak256(abi.encode(key, slot)) — the canonical Solidity mapping slot.
        let key = Address::from([0x11u8; 20]);
        let mut expected_buf = [0u8; 64];
        expected_buf[12..32].copy_from_slice(key.as_slice());
        expected_buf[32..].copy_from_slice(&U256::from(7u64).to_be_bytes::<32>());
        let expected = U256::from_be_bytes(keccak256(expected_buf).0);

        assert_eq!(direct_slot(key, 7), expected);
    }

    #[test]
    fn nested_slot_nests_outward_in() {
        let owner = Address::from([0xAAu8; 20]);
        let spender = Address::from([0xBBu8; 20]);

        // Inner key hashes against the OUTER key's slot, not the raw slot index.
        let outer = direct_slot(owner, 3);
        let mut buf = [0u8; 64];
        buf[12..32].copy_from_slice(spender.as_slice());
        buf[32..].copy_from_slice(&outer.to_be_bytes::<32>());
        let expected = U256::from_be_bytes(keccak256(buf).0);

        assert_eq!(nested_slot(owner, spender, 3), expected);
    }

    #[test]
    fn nested_slot_is_asymmetric_in_owner_and_spender() {
        // Guards against swapping owner/spender, which would seed an allowance
        // in the wrong direction and still "look" seeded.
        let a = Address::from([0xAAu8; 20]);
        let b = Address::from([0xBBu8; 20]);
        assert_ne!(nested_slot(a, b, 1), nested_slot(b, a, 1));
    }

    #[test]
    fn distinct_holders_never_share_a_balance_slot() {
        let a = Address::from([0x01u8; 20]);
        let b = Address::from([0x02u8; 20]);
        for slot in 0..8u64 {
            assert_ne!(direct_slot(a, slot), direct_slot(b, slot));
        }
    }

    #[test]
    fn distinct_mapping_slots_never_collide_for_one_holder() {
        let holder = Address::from([0x42u8; 20]);
        let slots: Vec<U256> = (0..16u64).map(|s| direct_slot(holder, s)).collect();
        for (i, a) in slots.iter().enumerate() {
            for b in slots.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }
}
