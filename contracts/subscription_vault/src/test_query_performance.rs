#![cfg(test)]

use crate::{
    queries::{MAX_SCAN_DEPTH, MAX_SUBSCRIPTION_LIST_PAGE},
    subscription::MAX_WRITE_PATH_SCAN_DEPTH,
    types::{Subscription, SubscriptionStatus},
    SubscriptionVault, SubscriptionVaultClient,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, Symbol,
};

const T0: u64 = 1700000000;

fn setup() -> (Env, SubscriptionVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(T0);
    // Needed to avoid gas limits when doing deep mock pagination in tests
    env.cost_estimate().budget().reset_unlimited();

    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    client.init(&token, &6, &admin, &1_000_000i128, &(7 * 24 * 60 * 60));

    (env, client, token, admin)
}

fn create_mock_sub(env: &Env, subscriber: &Address, token: &Address) -> Subscription {
    Subscription {
        subscriber: subscriber.clone(),
        merchant: Address::generate(env),
        token: token.clone(),
        amount: 10_000,
        interval_seconds: 2_592_000,
        last_payment_timestamp: env.ledger().timestamp(),
        status: SubscriptionStatus::Active,
        prepaid_balance: 0,
        usage_enabled: false,
        lifetime_cap: None,
        lifetime_charged: 0,
        start_time: env.ledger().timestamp(),
        expires_at: None,
        grace_start_timestamp: None,
    }
}

/// Helper to quickly inject N subscriptions directly into storage without crossing the host boundary repeatedly
fn inject_subscriptions(
    env: &Env,
    contract_id: &Address,
    count: u32,
    subscriber: &Address,
    token: &Address,
) {
    env.as_contract(contract_id, || {
        let next_id_key = Symbol::new(env, "next_id");
        let start_id: u32 = env.storage().instance().get(&next_id_key).unwrap_or(0);

        for i in 0..count {
            let id = start_id + i;
            let sub = create_mock_sub(env, subscriber, token);
            env.storage().instance().set(&id, &sub);
        }

        env.storage()
            .instance()
            .set(&next_id_key, &(start_id + count));
    });
}

#[test]
fn test_subscriber_list_basic() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    inject_subscriptions(&env, &client.address, 5, &subscriber, &token);

    let page = client.list_subscriptions_by_subscriber(&subscriber, &0, &100);
    assert_eq!(page.subscription_ids.len(), 5);
    assert_eq!(page.next_start_id, None);
}

#[test]
fn test_subscriber_list_pagination() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    inject_subscriptions(&env, &client.address, 50, &subscriber, &token);

    // Fetch first 20
    let page1 = client.list_subscriptions_by_subscriber(&subscriber, &0, &20);
    assert_eq!(page1.subscription_ids.len(), 20);
    assert_eq!(page1.next_start_id, Some(20));

    // Fetch next 20
    let page2 = client.list_subscriptions_by_subscriber(&subscriber, &page1.next_start_id.unwrap(), &20);
    assert_eq!(page2.subscription_ids.len(), 20);
    assert_eq!(page2.next_start_id, Some(40));

    // Fetch last 10
    let page3 = client.list_subscriptions_by_subscriber(&subscriber, &page2.next_start_id.unwrap(), &20);
    assert_eq!(page3.subscription_ids.len(), 10);
    // next_id is 50, scan budget doesn't exhaust and it found all, so next_start_id should be None
    assert_eq!(page3.next_start_id, None);
}

#[test]
fn test_subscriber_list_scan_depth_boundary() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    let other = Address::generate(&env);

    // Create exactly MAX_SCAN_DEPTH + 10 subscriptions, all for `other`
    let total = MAX_SCAN_DEPTH + 10;
    inject_subscriptions(&env, &client.address, total, &other, &token);

    // Now if `subscriber` tries to list, it will scan MAX_SCAN_DEPTH IDs, find none,
    // and return an empty list WITH a next_start_id cursor to resume at MAX_SCAN_DEPTH.
    let page1 = client.list_subscriptions_by_subscriber(&subscriber, &0, &10);
    assert_eq!(page1.subscription_ids.len(), 0);
    assert_eq!(page1.next_start_id, Some(MAX_SCAN_DEPTH));

    let page2 = client.list_subscriptions_by_subscriber(&subscriber, &page1.next_start_id.unwrap(), &10);
    assert_eq!(page2.subscription_ids.len(), 0);
    assert_eq!(page2.next_start_id, None); // Finished remaining 10
}

#[test]
fn test_subscriber_list_sparse_ids() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    let other = Address::generate(&env);

    inject_subscriptions(&env, &client.address, 10, &subscriber, &token);
    inject_subscriptions(&env, &client.address, 40, &other, &token);
    inject_subscriptions(&env, &client.address, 10, &subscriber, &token);

    // 60 total subscriptions. subscriber has 0..10 and 50..60.
    let page = client.list_subscriptions_by_subscriber(&subscriber, &0, &100);
    assert_eq!(page.subscription_ids.len(), 20);
    assert_eq!(page.next_start_id, None);
}

#[test]
fn test_subscriber_list_limit_one() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    inject_subscriptions(&env, &client.address, 5, &subscriber, &token);

    let page = client.list_subscriptions_by_subscriber(&subscriber, &0, &1);
    assert_eq!(page.subscription_ids.len(), 1);
    assert_eq!(page.subscription_ids.get(0).unwrap(), 0);
    assert_eq!(page.next_start_id, Some(1));
}

#[test]
fn test_subscriber_list_limit_max() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    inject_subscriptions(&env, &client.address, MAX_SUBSCRIPTION_LIST_PAGE, &subscriber, &token);

    let page = client.list_subscriptions_by_subscriber(&subscriber, &0, &MAX_SUBSCRIPTION_LIST_PAGE);
    assert_eq!(page.subscription_ids.len(), MAX_SUBSCRIPTION_LIST_PAGE);
    // Note: since it hit the limit exactly on the last item, it might return next_start_id == Some(100) or None
    // Currently, it breaks early, so if loop finishes, it sets to None. Wait, if it pushes max, len == limit. Next iteration breaks.
    // We just ensure it doesn't crash.
}

#[test]
fn test_subscriber_list_empty() {
    let (env, client, _token, _) = setup();
    let subscriber = Address::generate(&env);
    let page = client.list_subscriptions_by_subscriber(&subscriber, &0, &100);
    assert_eq!(page.subscription_ids.len(), 0);
    assert_eq!(page.next_start_id, None);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_subscriber_list_invalid_limit_zero() {
    let (env, client, _token, _) = setup();
    client.list_subscriptions_by_subscriber(&Address::generate(&env), &0, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_subscriber_list_invalid_limit_overflow() {
    let (env, client, _token, _) = setup();
    client.list_subscriptions_by_subscriber(&Address::generate(&env), &0, &(MAX_SUBSCRIPTION_LIST_PAGE + 1));
}

fn create_sub_for_merchant_and_token(client: &SubscriptionVaultClient<'static>, subscriber: &Address, merchant: &Address, token: &Address) -> u32 {
    client.create_subscription(subscriber, merchant, &1000, &(30 * 24 * 60 * 60), &false, &None, &None::<u64>)
}

#[test]
fn test_merchant_query_basic() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    for _ in 0..10 {
        create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    }

    let page = client.get_subscriptions_by_merchant(&merchant, &0, &100);
    assert_eq!(page.len(), 10);
    assert_eq!(client.get_merchant_subscription_count(&merchant), 10);
}

#[test]
fn test_merchant_query_pagination() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    for _ in 0..15 {
        create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    }

    let page1 = client.get_subscriptions_by_merchant(&merchant, &0, &10);
    assert_eq!(page1.len(), 10);

    let page2 = client.get_subscriptions_by_merchant(&merchant, &10, &10);
    assert_eq!(page2.len(), 5);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_merchant_query_limit_zero() {
    let (env, client, _token, _) = setup();
    client.get_subscriptions_by_merchant(&Address::generate(&env), &0, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_merchant_query_limit_overflow() {
    let (env, client, _token, _) = setup();
    client.get_subscriptions_by_merchant(&Address::generate(&env), &0, &(MAX_SUBSCRIPTION_LIST_PAGE + 1));
}

#[test]
fn test_merchant_query_start_past_end() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    
    let page = client.get_subscriptions_by_merchant(&merchant, &2, &10);
    assert_eq!(page.len(), 0);
}

#[test]
fn test_token_query_basic() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    for _ in 0..10 {
        create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    }

    let page = client.get_subscriptions_by_token(&token, &0, &100);
    assert_eq!(page.len(), 10);
    assert_eq!(client.get_token_subscription_count(&token), 10);
}

#[test]
fn test_token_query_pagination() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    for _ in 0..15 {
        create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    }

    let page1 = client.get_subscriptions_by_token(&token, &0, &10);
    assert_eq!(page1.len(), 10);

    let page2 = client.get_subscriptions_by_token(&token, &10, &10);
    assert_eq!(page2.len(), 5);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_token_query_limit_zero() {
    let (env, client, token, _) = setup();
    client.get_subscriptions_by_token(&token, &0, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_token_query_limit_overflow() {
    let (env, client, token, _) = setup();
    client.get_subscriptions_by_token(&token, &0, &(MAX_SUBSCRIPTION_LIST_PAGE + 1));
}

#[test]
fn test_merchant_count_and_token_count() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    assert_eq!(client.get_merchant_subscription_count(&merchant), 0);
    assert_eq!(client.get_token_subscription_count(&token), 0);

    create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    
    assert_eq!(client.get_merchant_subscription_count(&merchant), 1);
    assert_eq!(client.get_token_subscription_count(&token), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_write_path_scan_depth_guard_triggers_for_large_contracts() {
    let (env, client, token, _) = setup();
    
    // We simulate a contract that has exceeded the MAX_WRITE_PATH_SCAN_DEPTH
    // by injecting a fake next_id. 
    env.as_contract(&client.address, || {
        env.storage().instance().set(&Symbol::new(&env, "next_id"), &(MAX_WRITE_PATH_SCAN_DEPTH + 1));
    });

    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);

    // In order to trigger the O(n) scan, we need a credit limit > 0
    // so `compute_subscriber_exposure` gets called instead of fast-path exiting.
    env.as_contract(&client.address, || {
        let credit_limit_key = (Symbol::new(&env, "credit_limit"), subscriber.clone(), token.clone());
        env.storage().instance().set(&credit_limit_key, &1000i128); // Non-zero sets up the scan
    });

    // This creation should fail with InvalidInput because we simulated an oversized contract
    // AND we forced the scan path by configuring a credit limit.
    client.create_subscription(&subscriber, &merchant, &100, &(30 * 24 * 60 * 60), &false, &None, &None::<u64>);
}

// ── Deterministic ordering ────────────────────────────────────────────────────

#[test]
fn test_subscriber_list_results_are_ascending() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    inject_subscriptions(&env, &client.address, 20, &subscriber, &token);

    let page = client.list_subscriptions_by_subscriber(&subscriber, &0, &20);
    assert_eq!(page.subscription_ids.len(), 20);

    // IDs must be in strictly ascending order.
    let mut prev = page.subscription_ids.get(0).unwrap();
    let mut i = 1u32;
    while i < page.subscription_ids.len() {
        let current = page.subscription_ids.get(i).unwrap();
        assert!(current > prev, "IDs must be ascending: {} <= {}", current, prev);
        prev = current;
        i += 1;
    }
}

#[test]
fn test_merchant_query_results_are_stable_across_pages() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    for _ in 0..30 {
        create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    }

    let total = client.get_merchant_subscription_count(&merchant);
    assert_eq!(total, 30);

    // Two consecutive pages must account for all subscriptions without gaps.
    let page1 = client.get_subscriptions_by_merchant(&merchant, &0, &15);
    let page2 = client.get_subscriptions_by_merchant(&merchant, &15, &15);
    assert_eq!(page1.len(), 15);
    assert_eq!(page2.len(), 15);
    assert_eq!(page1.len() + page2.len(), total);

    // A third page past the end must be empty.
    let page3 = client.get_subscriptions_by_merchant(&merchant, &30, &15);
    assert_eq!(page3.len(), 0);
}

// ── Multi-subscriber isolation ────────────────────────────────────────────────

#[test]
fn test_subscriber_isolation_no_cross_contamination() {
    let (env, client, token, _) = setup();
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);

    // Interleave: 5 alice, 5 bob, 5 alice.
    inject_subscriptions(&env, &client.address, 5, &alice, &token);
    inject_subscriptions(&env, &client.address, 5, &bob, &token);
    inject_subscriptions(&env, &client.address, 5, &alice, &token);

    let alice_page = client.list_subscriptions_by_subscriber(&alice, &0, &100);
    let bob_page = client.list_subscriptions_by_subscriber(&bob, &0, &100);

    assert_eq!(alice_page.subscription_ids.len(), 10);
    assert_eq!(bob_page.subscription_ids.len(), 5);

    // None of bob's IDs should appear in alice's result.
    let mut a = 0u32;
    while a < alice_page.subscription_ids.len() {
        let alice_id = alice_page.subscription_ids.get(a).unwrap();
        let mut b = 0u32;
        while b < bob_page.subscription_ids.len() {
            let bob_id = bob_page.subscription_ids.get(b).unwrap();
            assert_ne!(alice_id, bob_id, "alice_id {} appeared in bob's result", alice_id);
            b += 1;
        }
        a += 1;
    }
}

// ── Exact-multiple-of-limit edge case ────────────────────────────────────────

#[test]
fn test_subscriber_list_exact_multiple_of_limit() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    // Exactly 3 pages of 10.
    inject_subscriptions(&env, &client.address, 30, &subscriber, &token);

    let page1 = client.list_subscriptions_by_subscriber(&subscriber, &0, &10);
    assert_eq!(page1.subscription_ids.len(), 10);
    // Should have a resume cursor since there are more IDs.
    assert!(page1.next_start_id.is_some());

    let page2 = client.list_subscriptions_by_subscriber(&subscriber, &page1.next_start_id.unwrap(), &10);
    assert_eq!(page2.subscription_ids.len(), 10);
    assert!(page2.next_start_id.is_some());

    let page3 = client.list_subscriptions_by_subscriber(&subscriber, &page2.next_start_id.unwrap(), &10);
    assert_eq!(page3.subscription_ids.len(), 10);
    // Last page, no more IDs.
    assert_eq!(page3.next_start_id, None);
}

// ── start_from_id beyond last subscription ────────────────────────────────────

#[test]
fn test_subscriber_list_start_beyond_range() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    inject_subscriptions(&env, &client.address, 5, &subscriber, &token);

    // start_from_id well past the highest allocated ID.
    let page = client.list_subscriptions_by_subscriber(&subscriber, &1000, &10);
    assert_eq!(page.subscription_ids.len(), 0);
    assert_eq!(page.next_start_id, None);
}

// ── Merchant: start offset exactly at end ────────────────────────────────────

#[test]
fn test_merchant_query_start_at_exact_end() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    for _ in 0..5 {
        create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    }

    // Offset == count returns empty (not an error).
    let page = client.get_subscriptions_by_merchant(&merchant, &5, &10);
    assert_eq!(page.len(), 0);
}

// ── Billing statement pagination ──────────────────────────────────────────────

/// Inject `count` billing statements for `sub_id` directly into contract storage,
/// bypassing the charge path and any token/balance requirements.
fn inject_statements(env: &Env, contract_id: &Address, sub_id: u32, count: u32, merchant: &Address) {
    env.as_contract(contract_id, || {
        let interval: u64 = 30 * 24 * 60 * 60;
        for i in 0..count {
            crate::statements::append_statement(
                env,
                sub_id,
                10_000,
                merchant.clone(),
                crate::types::BillingChargeKind::Interval,
                T0 + interval * i as u64,
                T0 + interval * (i + 1) as u64,
            ).unwrap();
        }
    });
}

#[test]
fn test_statements_empty_subscription() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);

    let sub_id = create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);

    // Offset style: zero statements should return empty page.
    let page = client.get_sub_statements_offset(&sub_id, &0, &10, &true);
    assert_eq!(page.statements.len(), 0);
    assert_eq!(page.next_cursor, None);
    assert_eq!(page.total, 0);

    // Cursor style: same.
    let page2 = client.get_sub_statements_cursor(&sub_id, &None::<u32>, &10, &true);
    assert_eq!(page2.statements.len(), 0);
    assert_eq!(page2.next_cursor, None);
    assert_eq!(page2.total, 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_statements_offset_limit_zero() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let sub_id = create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    client.get_sub_statements_offset(&sub_id, &0, &0, &true);
}

#[test]
#[should_panic(expected = "Error(Contract, #1015)")] // InvalidInput = 1015
fn test_statements_cursor_limit_zero() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let sub_id = create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
    client.get_sub_statements_cursor(&sub_id, &None::<u32>, &0, &true);
}

#[test]
fn test_statements_offset_and_cursor_consistent() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let sub_id = create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);

    // Inject 5 statements directly to avoid deposit/token setup.
    inject_statements(&env, &client.address, sub_id, 5, &merchant);

    let total_page = client.get_sub_statements_offset(&sub_id, &0, &100, &false);
    assert_eq!(total_page.total, 5);
    assert_eq!(total_page.statements.len(), 5);

    // Paginate in groups of 2 and reconstruct.
    let p1 = client.get_sub_statements_offset(&sub_id, &0, &2, &false);
    assert_eq!(p1.statements.len(), 2);
    assert!(p1.next_cursor.is_some());

    let p2 = client.get_sub_statements_cursor(&sub_id, &p1.next_cursor, &2, &false);
    assert_eq!(p2.statements.len(), 2);
    assert!(p2.next_cursor.is_some());

    let p3 = client.get_sub_statements_cursor(&sub_id, &p2.next_cursor, &2, &false);
    assert_eq!(p3.statements.len(), 1);
    assert_eq!(p3.next_cursor, None);

    // Sequences must be monotonically increasing when oldest_first.
    let mut prev_seq = p1.statements.get(0).unwrap().sequence;
    let mut idx = 1u32;
    while idx < p1.statements.len() {
        let seq = p1.statements.get(idx).unwrap().sequence;
        assert!(seq > prev_seq);
        prev_seq = seq;
        idx += 1;
    }
}

#[test]
fn test_statements_newest_first_reverses_order() {
    let (env, client, token, _) = setup();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let sub_id = create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);

    // Inject 4 statements directly to avoid deposit/token setup.
    inject_statements(&env, &client.address, sub_id, 4, &merchant);

    let oldest_first = client.get_sub_statements_offset(&sub_id, &0, &10, &false);
    let newest_first = client.get_sub_statements_offset(&sub_id, &0, &10, &true);

    assert_eq!(oldest_first.total, newest_first.total);
    let n = oldest_first.statements.len();
    assert!(n > 0);

    // Sequences must be in reverse order when newest_first.
    let mut i = 0u32;
    while i < n {
        let old_seq = oldest_first.statements.get(i).unwrap().sequence;
        let new_seq = newest_first.statements.get(n - 1 - i).unwrap().sequence;
        assert_eq!(old_seq, new_seq, "Sequence mismatch at index {}", i);
        i += 1;
    }
}

// ── Token query pagination ────────────────────────────────────────────────────

#[test]
fn test_token_query_start_past_end() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);

    let page = client.get_subscriptions_by_token(&token, &5, &10);
    assert_eq!(page.len(), 0);
}

#[test]
fn test_token_query_count_increments() {
    let (env, client, token, _) = setup();
    let merchant = Address::generate(&env);
    let subscriber = Address::generate(&env);

    assert_eq!(client.get_token_subscription_count(&token), 0);
    for expected in 1u32..=5 {
        create_sub_for_merchant_and_token(&client, &subscriber, &merchant, &token);
        assert_eq!(client.get_token_subscription_count(&token), expected);
    }
}
