use crate::{
    can_transition, compute_next_charge_info, get_allowed_transitions, validate_status_transition,
    ChargeExecutionResult, Error, MerchantWithdrawalEvent, OraclePrice, RecoveryReason,
    Subscription, SubscriptionStatus, SubscriptionVault, SubscriptionVaultClient,
    MAX_SUBSCRIPTION_ID, MAX_SUBSCRIPTION_LIST_PAGE,
};
use soroban_sdk::testutils::{Address as _, Events, Ledger as _};
use soroban_sdk::{
    contract, contractimpl, Address, Env, FromVal, IntoVal, String, Symbol, TryFromVal, Val, Vec,
};

extern crate alloc;
use crate::test_utils::{assertions, fixtures, setup::TestEnv};
use alloc::format;

// -- constants ----------------------------------------------------------------
const T0: u64 = 1_000;
const INTERVAL: u64 = 30 * 24 * 60 * 60; // 30 days
const AMOUNT: i128 = 10_000_000; // 10 USDC (6 decimals)
const PREPAID: i128 = 50_000_000; // 50 USDC

// -- lifecycle action enum for property tests --------------------------------
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LifecycleAction {
    Pause,
    Resume,
    Cancel,
}

// -- all subscription statuses for property tests ----------------------------
const ALL_STATUSES: &[SubscriptionStatus] = &[
    SubscriptionStatus::Active,
    SubscriptionStatus::Paused,
    SubscriptionStatus::Cancelled,
    SubscriptionStatus::InsufficientBalance,
    SubscriptionStatus::GracePeriod,
];

// -- helpers ------------------------------------------------------------------

fn create_token_and_mint(env: &Env, recipient: &Address, amount: i128) -> Address {
    let token_admin = Address::generate(env);
    let token_addr = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    let token_client = soroban_sdk::token::StellarAssetClient::new(env, &token_addr);
    token_client.mint(recipient, &amount);
    token_addr
}

/// Standard setup: mock auth, register contract, init with real token + 7-day grace.
fn setup_test_env() -> (Env, SubscriptionVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let min_topup = 1_000_000i128; // 1 USDC
    client.init(&token, &6, &admin, &min_topup, &(7 * 24 * 60 * 60));

    (env, client, token, admin)
}

/// Helper used by reentrancy tests: returns (client, token, admin) with env pre-configured.
fn setup_contract(env: &Env) -> (SubscriptionVaultClient<'_>, Address, Address) {
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(env, &contract_id);
    let admin = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    client.init(&token, &6, &admin, &1_000_000i128, &(7 * 24 * 60 * 60));
    (client, token, admin)
}

/// Create a test subscription, then patch its status for direct-manipulation tests.
fn create_test_subscription(
    env: &Env,
    client: &SubscriptionVaultClient,
    status: SubscriptionStatus,
) -> (u32, Address, Address) {
    let subscriber = Address::generate(env);
    let merchant = Address::generate(env);
    // FIXED: Removed the extra &None::<u64> (now 7 arguments)
    let id = client.create_subscription(
        &subscriber,
        &merchant,
        &AMOUNT,
        &INTERVAL,
        &false,
        &None::<i128>,
        &None::<u64>,
    );
    if status != SubscriptionStatus::Active {
        let mut sub = client.get_subscription(&id);
        sub.status = status;
        env.as_contract(&client.address, || {
            env.storage().instance().set(&id, &sub);
        });
    }
    (id, subscriber, merchant)
}

/// Seed a subscription with a known prepaid balance directly in storage.
fn seed_balance(env: &Env, client: &SubscriptionVaultClient, id: u32, balance: i128) {
    let mut sub = client.get_subscription(&id);
    sub.prepaid_balance = balance;
    env.as_contract(&client.address, || {
        env.storage().instance().set(&id, &sub);
    });
}

/// Seed the `next_id` counter to an arbitrary value.
fn seed_counter(env: &Env, contract_id: &Address, value: u32) {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .set(&soroban_sdk::Symbol::new(env, "next_id"), &value);
    });
}

fn seed_merchant_balance(
    env: &Env,
    contract_id: &Address,
    merchant: &Address,
    token: &Address,
    balance: i128,
) {
    env.as_contract(contract_id, || {
        env.storage().instance().set(
            &(
                Symbol::new(env, "merchant_balance"),
                merchant.clone(),
                token.clone(),
            ),
            &balance,
        );
    });
}

fn create_secondary_token(env: &Env) -> Address {
    env.register_stellar_asset_contract_v2(Address::generate(env))
        .address()
}

fn snapshot_subscriptions(
    client: &SubscriptionVaultClient,
    ids: &[u32],
) -> alloc::vec::Vec<Subscription> {
    ids.iter().map(|id| client.get_subscription(id)).collect()
}

fn collect_batch_result_codes(
    env: &Env,
    client: &SubscriptionVaultClient,
    ids: &[u32],
) -> alloc::vec::Vec<(bool, u32)> {
    let ids_vec = ids.iter().fold(Vec::<u32>::new(env), |mut acc, id| {
        acc.push_back(*id);
        acc
    });
    let results = client.batch_charge(&ids_vec);
    results
        .iter()
        .map(|result| (result.success, result.error_code))
        .collect()
}

fn collect_single_charge_result_codes(
    client: &SubscriptionVaultClient,
    ids: &[u32],
) -> alloc::vec::Vec<(bool, u32)> {
    ids.iter()
        .map(|id| match client.try_charge_subscription(id) {
            Ok(Ok(ChargeExecutionResult::Charged)) => (true, 0),
            Ok(Ok(ChargeExecutionResult::InsufficientBalance)) => {
                (false, Error::InsufficientBalance.to_code())
            }
            Err(Ok(err)) => (false, err.to_code()),
            other => panic!("unexpected charge result: {other:?}"),
        })
        .collect()
}

#[contract]
struct MockOracle;

#[contractimpl]
impl MockOracle {
    pub fn set_price(env: Env, price: i128, timestamp: u64) {
        env.storage().instance().set(
            &Symbol::new(&env, "price"),
            &OraclePrice { price, timestamp },
        );
    }

    pub fn latest_price(env: Env) -> OraclePrice {
        env.storage()
            .instance()
            .get(&Symbol::new(&env, "price"))
            .unwrap_or(OraclePrice {
                price: 0,
                timestamp: 0,
            })
    }
}

fn lcg_next(seed: &mut u64) -> u64 {
    const A: u64 = 1664525;
    const C: u64 = 1013904223;
    *seed = seed.wrapping_mul(A).wrapping_add(C);
    *seed
}

fn manual_can_transition(from: &SubscriptionStatus, to: &SubscriptionStatus) -> bool {
    use SubscriptionStatus::*;

    if from == to {
        return true;
    }

    match (from, to) {
        (Active, Paused) => true,
        (Active, Cancelled) => true,
        (Active, InsufficientBalance) => true,
        (Active, GracePeriod) => true,
        (Paused, Active) => true,
        (Paused, Cancelled) => true,
        (InsufficientBalance, Active) => true,
        (InsufficientBalance, Cancelled) => true,
        (GracePeriod, Active) => true,
        (GracePeriod, Cancelled) => true,
        (GracePeriod, InsufficientBalance) => true,
        _ => false,
    }
}

fn random_transition_action(seed: &mut u64) -> u32 {
    (lcg_next(seed) % 5) as u32
}

fn transition_action_target(action: u32) -> SubscriptionStatus {
    match action % 5 {
        0 => SubscriptionStatus::Active,
        1 => SubscriptionStatus::Paused,
        2 => SubscriptionStatus::Cancelled,
        3 => SubscriptionStatus::InsufficientBalance,
        _ => SubscriptionStatus::GracePeriod,
    }
}

fn random_lifecycle_action(seed: &mut u64) -> LifecycleAction {
    match lcg_next(seed) % 3 {
        0 => LifecycleAction::Pause,
        1 => LifecycleAction::Resume,
        _ => LifecycleAction::Cancel,
    }
}

fn lifecycle_action_target(action: LifecycleAction) -> SubscriptionStatus {
    match action {
        LifecycleAction::Pause => SubscriptionStatus::Paused,
        LifecycleAction::Resume => SubscriptionStatus::Active,
        LifecycleAction::Cancel => SubscriptionStatus::Cancelled,
    }
}

// ── State Machine Helper Tests ─────────────────────────────────────────────────

#[test]
fn test_validate_status_transition_same_status_is_allowed() {
    assert!(
        validate_status_transition(&SubscriptionStatus::Active, &SubscriptionStatus::Active)
            .is_ok()
    );
    assert!(
        validate_status_transition(&SubscriptionStatus::Paused, &SubscriptionStatus::Paused)
            .is_ok()
    );
    assert!(validate_status_transition(
        &SubscriptionStatus::Cancelled,
        &SubscriptionStatus::Cancelled
    )
    .is_ok());
    assert!(validate_status_transition(
        &SubscriptionStatus::InsufficientBalance,
        &SubscriptionStatus::InsufficientBalance
    )
    .is_ok());
}

#[test]
fn test_validate_active_transitions() {
    assert!(
        validate_status_transition(&SubscriptionStatus::Active, &SubscriptionStatus::Paused)
            .is_ok()
    );
    assert!(validate_status_transition(
        &SubscriptionStatus::Active,
        &SubscriptionStatus::Cancelled
    )
    .is_ok());
    assert!(validate_status_transition(
        &SubscriptionStatus::Active,
        &SubscriptionStatus::InsufficientBalance
    )
    .is_ok());
}

#[test]
fn test_validate_paused_transitions() {
    assert!(
        validate_status_transition(&SubscriptionStatus::Paused, &SubscriptionStatus::Active)
            .is_ok()
    );
    assert!(validate_status_transition(
        &SubscriptionStatus::Paused,
        &SubscriptionStatus::Cancelled
    )
    .is_ok());
    assert_eq!(
        validate_status_transition(
            &SubscriptionStatus::Paused,
            &SubscriptionStatus::InsufficientBalance
        ),
        Err(Error::InvalidStatusTransition)
    );
}

#[test]
fn test_validate_insufficient_balance_transitions() {
    assert!(validate_status_transition(
        &SubscriptionStatus::InsufficientBalance,
        &SubscriptionStatus::Active
    )
    .is_ok());
    assert!(validate_status_transition(
        &SubscriptionStatus::InsufficientBalance,
        &SubscriptionStatus::Cancelled
    )
    .is_ok());
    assert_eq!(
        validate_status_transition(
            &SubscriptionStatus::InsufficientBalance,
            &SubscriptionStatus::Paused
        ),
        Err(Error::InvalidStatusTransition)
    );
}

#[test]
fn test_validate_cancelled_transitions_all_blocked() {
    assert_eq!(
        validate_status_transition(&SubscriptionStatus::Cancelled, &SubscriptionStatus::Active),
        Err(Error::InvalidStatusTransition)
    );
    assert_eq!(
        validate_status_transition(&SubscriptionStatus::Cancelled, &SubscriptionStatus::Paused),
        Err(Error::InvalidStatusTransition)
    );
    assert_eq!(
        validate_status_transition(
            &SubscriptionStatus::Cancelled,
            &SubscriptionStatus::InsufficientBalance
        ),
        Err(Error::InvalidStatusTransition)
    );
}

#[test]
fn test_can_transition_helper() {
    assert!(can_transition(
        &SubscriptionStatus::Active,
        &SubscriptionStatus::Paused
    ));
    assert!(can_transition(
        &SubscriptionStatus::Active,
        &SubscriptionStatus::Cancelled
    ));
    assert!(can_transition(
        &SubscriptionStatus::Paused,
        &SubscriptionStatus::Active
    ));
    assert!(!can_transition(
        &SubscriptionStatus::Cancelled,
        &SubscriptionStatus::Active
    ));
    assert!(!can_transition(
        &SubscriptionStatus::Cancelled,
        &SubscriptionStatus::Paused
    ));
    assert!(!can_transition(
        &SubscriptionStatus::Paused,
        &SubscriptionStatus::InsufficientBalance
    ));
}

#[test]
fn test_get_allowed_transitions() {
    let active_targets = get_allowed_transitions(&SubscriptionStatus::Active);
    assert!(active_targets.contains(&SubscriptionStatus::Paused));
    assert!(active_targets.contains(&SubscriptionStatus::Cancelled));
    assert!(active_targets.contains(&SubscriptionStatus::InsufficientBalance));

    let paused_targets = get_allowed_transitions(&SubscriptionStatus::Paused);
    assert_eq!(paused_targets.len(), 3);
    assert!(paused_targets.contains(&SubscriptionStatus::Active));
    assert!(paused_targets.contains(&SubscriptionStatus::Cancelled));
    assert!(paused_targets.contains(&SubscriptionStatus::Expired));

    assert_eq!(
        get_allowed_transitions(&SubscriptionStatus::Cancelled).len(),
        1
    );

    let ib_targets = get_allowed_transitions(&SubscriptionStatus::InsufficientBalance);
    assert_eq!(ib_targets.len(), 3);
}

#[test]
fn test_state_machine_property_transition_matrix_matches_manual_rules() {
    for from in ALL_STATUSES.iter() {
        let allowed = get_allowed_transitions(from);

        for to in ALL_STATUSES.iter() {
            let expected = manual_can_transition(from, to);
            assert_eq!(can_transition(from, to), expected);
            assert_eq!(validate_status_transition(from, to).is_ok(), expected);

            if from == to {
                assert!(!allowed.contains(to));
            } else {
                assert_eq!(allowed.contains(to), expected);
            }
        }
    }
}

#[test]
fn test_state_machine_property_random_transition_sequences_only_allow_legal_targets() {
    for start in ALL_STATUSES.iter() {
        for seed_base in 0..64u64 {
            let mut seed = seed_base + (start.clone() as u64) * 97;
            let mut current = start.clone();

            for _ in 0..24 {
                let action = random_transition_action(&mut seed);
                let target = transition_action_target(action);
                let expected = manual_can_transition(&current, &target);

                assert_eq!(can_transition(&current, &target), expected);
                assert_eq!(
                    validate_status_transition(&current, &target).is_ok(),
                    expected
                );

                if expected {
                    current = target;
                }
            }
        }
    }
}

#[test]
fn test_state_machine_property_lifecycle_entrypoints_follow_manual_model() {
    for start in ALL_STATUSES.iter() {
        for seed_base in 0..48u64 {
            let (env, client, token, _admin) = setup_test_env();
            let (id, subscriber, _) = create_test_subscription(&env, &client, start.clone());
            let mut expected = start.clone();
            let mut seed = seed_base + (start.clone() as u64) * 131;

            for _ in 0..12 {
                let action = random_lifecycle_action(&mut seed);
                let target = lifecycle_action_target(action);
                let should_succeed = manual_can_transition(&expected, &target);

                if action == LifecycleAction::Resume
                    && (expected == SubscriptionStatus::InsufficientBalance
                        || expected == SubscriptionStatus::GracePeriod)
                    && should_succeed
                {
                    let token_client = soroban_sdk::token::StellarAssetClient::new(&env, &token);
                    token_client.mint(&subscriber, &AMOUNT);
                    client.deposit_funds(&id, &subscriber, &AMOUNT);
                }

                let result = match action {
                    LifecycleAction::Pause => client.try_pause_subscription(&id, &subscriber),
                    LifecycleAction::Resume => client.try_resume_subscription(&id, &subscriber),
                    LifecycleAction::Cancel => client.try_cancel_subscription(&id, &subscriber),
                };

                assert_eq!(result.is_ok(), should_succeed);

                let current = client.get_subscription(&id).status;
                if should_succeed {
                    expected = target;
                    assert_eq!(current, expected);
                } else {
                    assert_eq!(current, expected);
                }
            }
        }
    }
}

#[test]
fn test_state_machine_property_charge_failures_and_recovery_paths_obey_rules() {
    for seed_base in 0..32u64 {
        let mut seed = seed_base;

        for step in 0..10 {
            let (env, client, token, _) = setup_test_env();
            let (id, subscriber, _) =
                create_test_subscription(&env, &client, SubscriptionStatus::Active);
            let in_grace_window = lcg_next(&mut seed) % 2 == 0;
            let topup_amount = if lcg_next(&mut seed) % 2 == 0 {
                AMOUNT - 1
            } else {
                PREPAID
            };

            seed_balance(&env, &client, id, 0);
            let charge_time = if in_grace_window {
                T0 + INTERVAL + 1
            } else {
                T0 + INTERVAL + (7 * 24 * 60 * 60) + 1
            };
            env.ledger().set_timestamp(charge_time + step as u64);

            let result = client.try_charge_subscription(&id);
            assert_eq!(result, Ok(Ok(ChargeExecutionResult::InsufficientBalance)));

            let failed_status = client.get_subscription(&id).status;
            // Depending on charge_time, it could be GracePeriod or InsufficientBalance
            if in_grace_window {
                assert_eq!(failed_status, SubscriptionStatus::GracePeriod);
            } else {
                assert_eq!(failed_status, SubscriptionStatus::InsufficientBalance);
            }

            soroban_sdk::token::StellarAssetClient::new(&env, &token)
                .mint(&subscriber, &topup_amount.max(1_000_000));
            client.deposit_funds(&id, &subscriber, &topup_amount.max(1_000_000));

            let after_deposit = client.get_subscription(&id).status;
            if topup_amount >= AMOUNT {
                assert_eq!(after_deposit, SubscriptionStatus::Active);
            } else {
                assert!(
                    after_deposit == SubscriptionStatus::InsufficientBalance
                        || after_deposit == SubscriptionStatus::GracePeriod
                );
            }

            if topup_amount >= AMOUNT {
                env.ledger()
                    .set_timestamp(charge_time + INTERVAL + step as u64 + 1);
                let charge_again = client.try_charge_subscription(&id);
                assert!(charge_again.is_ok());
                assert_eq!(
                    client.get_subscription(&id).status,
                    SubscriptionStatus::Active
                );
            } else {
                client.cancel_subscription(&id, &subscriber);
                assert_eq!(
                    client.get_subscription(&id).status,
                    SubscriptionStatus::Cancelled
                );
            }
        }
    }
}

// -- Contract Lifecycle Tests -------------------------------------------------

#[test]
fn test_pause_subscription_from_active() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) = fixtures::create_subscription_detailed(
        &test_env.env,
        &test_env.client,
        SubscriptionStatus::Active,
        AMOUNT,
        INTERVAL,
    );
    test_env.client.pause_subscription(&id, &subscriber);
    assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Paused);
}

#[test]
#[should_panic(expected = "Error(Contract, #400)")]
fn test_pause_subscription_from_cancelled_should_fail() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) = fixtures::create_subscription_detailed(
        &test_env.env,
        &test_env.client,
        SubscriptionStatus::Active,
        AMOUNT,
        INTERVAL,
    );
    test_env.client.cancel_subscription(&id, &subscriber);
    test_env.client.pause_subscription(&id, &subscriber);
}

#[test]
fn test_pause_subscription_from_paused_is_idempotent() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) = fixtures::create_subscription_detailed(
        &test_env.env,
        &test_env.client,
        SubscriptionStatus::Active,
        AMOUNT,
        INTERVAL,
    );
    test_env.client.pause_subscription(&id, &subscriber);
    test_env.client.pause_subscription(&id, &subscriber);
    assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Paused);
}

#[test]
fn test_cancel_subscription_from_active() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) = fixtures::create_subscription_detailed(
        &test_env.env,
        &test_env.client,
        SubscriptionStatus::Active,
        AMOUNT,
        INTERVAL,
    );
    test_env.client.cancel_subscription(&id, &subscriber);
    assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Cancelled);
}

#[test]
fn test_cancel_subscription_from_paused() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) = fixtures::create_subscription_detailed(
        &test_env.env,
        &test_env.client,
        SubscriptionStatus::Active,
        AMOUNT,
        INTERVAL,
    );
    test_env.client.pause_subscription(&id, &subscriber);
    test_env.client.cancel_subscription(&id, &subscriber);
    assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Cancelled);
}

#[test]
fn test_cancel_subscription_from_cancelled_is_idempotent() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) = fixtures::create_subscription_detailed(
        &test_env.env,
        &test_env.client,
        SubscriptionStatus::Active,
        AMOUNT,
        INTERVAL,
    );
    test_env.client.cancel_subscription(&id, &subscriber);
    test_env.client.cancel_subscription(&id, &subscriber);
    assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Cancelled);
}

#[test]
fn test_resume_subscription_from_paused() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) = fixtures::create_subscription_detailed(
        &test_env.env,
        &test_env.client,
        SubscriptionStatus::Active,
        AMOUNT,
        INTERVAL,
    );
    test_env.client.pause_subscription(&id, &subscriber);
    test_env.client.resume_subscription(&id, &subscriber);
    assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Active);
}

#[test]
#[should_panic(expected = "Error(Contract, #400)")]
fn test_resume_subscription_from_cancelled_should_fail() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) =
        fixtures::create_subscription(&test_env.env, &test_env.client, SubscriptionStatus::Active);
    test_env.client.cancel_subscription(&id, &subscriber);
    test_env.client.resume_subscription(&id, &subscriber);
}

#[test]
fn test_full_lifecycle_active_pause_resume() {
    let test_env = TestEnv::default();
    let (id, subscriber, _) =
        fixtures::create_subscription(&test_env.env, &test_env.client, SubscriptionStatus::Active);
    test_env.client.pause_subscription(&id, &subscriber);
    assert_eq!(
        test_env.client.get_subscription(&id).status,
        SubscriptionStatus::Paused
    );
    test_env.client.resume_subscription(&id, &subscriber);
    assert_eq!(
        test_env.client.get_subscription(&id).status,
        SubscriptionStatus::Active
    );
    test_env.client.pause_subscription(&id, &subscriber);
    assert_eq!(
        test_env.client.get_subscription(&id).status,
        SubscriptionStatus::Paused
    );
}

#[test]
fn test_all_valid_transitions_coverage() {
    let test_env = TestEnv::default();

    // Active -> Paused
    {
        let (id, subscriber, _) = fixtures::create_subscription_detailed(
            &test_env.env,
            &test_env.client,
            SubscriptionStatus::Active,
            AMOUNT,
            INTERVAL,
        );
        test_env.client.pause_subscription(&id, &subscriber);
        assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Paused);
    }
    // Active -> Cancelled
    {
        let (id, subscriber, _) = fixtures::create_subscription_detailed(
            &test_env.env,
            &test_env.client,
            SubscriptionStatus::Active,
            AMOUNT,
            INTERVAL,
        );
        test_env.client.cancel_subscription(&id, &subscriber);
        assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Cancelled);
    }
    // Active -> InsufficientBalance (direct storage patch)
    {
        let (id, _, _) = fixtures::create_subscription_detailed(
            &test_env.env,
            &test_env.client,
            SubscriptionStatus::Active,
            AMOUNT,
            INTERVAL,
        );
        fixtures::patch_status(
            &test_env.env,
            &test_env.client,
            id,
            SubscriptionStatus::InsufficientBalance,
        );
        assertions::assert_status(
            &test_env.client,
            &id,
            SubscriptionStatus::InsufficientBalance,
        );
    }
    // Paused -> Active
    {
        let (id, subscriber, _) = fixtures::create_subscription_detailed(
            &test_env.env,
            &test_env.client,
            SubscriptionStatus::Active,
            AMOUNT,
            INTERVAL,
        );
        test_env.client.pause_subscription(&id, &subscriber);
        test_env.client.resume_subscription(&id, &subscriber);
        assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Active);
    }
    // Paused -> Cancelled
    {
        let (id, subscriber, _) = fixtures::create_subscription_detailed(
            &test_env.env,
            &test_env.client,
            SubscriptionStatus::Active,
            AMOUNT,
            INTERVAL,
        );
        test_env.client.pause_subscription(&id, &subscriber);
        test_env.client.cancel_subscription(&id, &subscriber);
        assertions::assert_status(&test_env.client, &id, SubscriptionStatus::Cancelled);
    }
}