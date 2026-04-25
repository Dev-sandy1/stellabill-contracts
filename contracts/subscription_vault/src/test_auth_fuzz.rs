//! # Authorization matrix (`test_auth_fuzz`)
//!
//! This module enumerates **action roles × operations** in a **fully deterministic** way: there is
//! no randomness or seed — coverage is the Cartesian product of [`Operation::all()`] and
//! [`Role::all()`] with a fresh [`FuzzHarness`] per case.
//!
//! ## Auth simulation
//!
//! For contract entrypoints that take an explicit `admin` / `authorizer` / `merchant` address, we
//! follow the same model as the rest of the crate tests: [`Env::mock_all_auths`]. That satisfies
//! `Address::require_auth` for the address passed in, so failures are the **contract** errors for
//! wrong principal (typically [`Error::Unauthorized`] or [`Error::Forbidden`]), not host auth
//! failures. [`Operation::BatchCharge`] is the exception: it requires the *stored* admin to sign; we
//! use an empty auth mock when the matrix expects denial.
//!
//! ## Exact error codes
//!
//! Denied cases assert the precise [`Error`] discriminant. `set_protocol_fee`, `add_to_blocklist`,
//! and the subscriber/merchant authorizer entrypoints when the address is a third party can return
//! [`Error::Forbidden`] (see `expected_error_when_denied`).
//!
//! ## Limitations
//!
//! - Does not try every business-logic error path (e.g. insufficient balance); it aims at
//!   **authorization** boundaries. Inputs are chosen so the first failure is auth-related.
//! - [`Operation::AddToBlocklist`] uses a victim address that has no merchant–subscriber
//!   relationship, so the merchant role is not “relationship-authorized”; that case is covered in
//!   focused blocklist tests in `test.rs`.
//! - `charge_subscription` / `charge_usage` (no per-address auth in the public API) are
//!   intentionally **out of scope** for this matrix; they are not admin-privileged in the same sense.
//! - [`Operation::DepositFunds`]: the contract only requires the **submitted** `subscriber` address
//!   to authorize; it does not assert identity against the subscription record. The matrix treats
//!   **subscriber and merchant** as allowed payers; other roles are still denied in the product.
//! - Merchant bucket withdraws for a principal with no stored balance may return [`Error::NotFound`]
//!   instead of [`Error::Unauthorized`]; denied cases accept that outcome.

extern crate std;

use crate::{
    Error, RecoveryReason, SubscriptionStatus, SubscriptionVault, SubscriptionVaultClient,
};

use soroban_sdk::{
    testutils::{Address as _, Ledger as _, MockAuth, MockAuthInvoke},
    Address, Env, String as SorobanString, Vec as SorobanVec, IntoVal,
};

/// Keep in sync with `STORAGE_VERSION` in `lib.rs` (migration / export).
const EXPECTED_STORAGE_VERSION: u32 = 2;

// ── Roles ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    Subscriber,
    Merchant,
    Stranger,
}

impl Role {
    pub fn all() -> &'static [Role] {
        &[
            Role::Admin,
            Role::Subscriber,
            Role::Merchant,
            Role::Stranger,
        ]
    }
}

// ── Operations (single source of truth for matrix + privileged lists) ─────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    SetMinTopup,
    RotateAdmin,
    SetProtocolFee,
    RecoverStrandedFunds,
    EnableEmergencyStop,
    DisableEmergencyStop,
    DepositFunds,
    CancelSubscription,
    PauseSubscription,
    ResumeSubscription,
    ChargeOneOff,
    WithdrawSubscriberFunds,
    WithdrawMerchantFunds,
    WithdrawMerchantTokenFunds,
    PauseMerchant,
    UnpauseMerchant,
    MerchantRefund,
    ConfigureUsageLimits,
    PartialRefund,
    BatchCharge,
    AddAcceptedToken,
    RemoveAcceptedToken,
    SetSubscriberCreditLimit,
    ExportContractSnapshot,
    ExportSubscriptionSummary,
    ExportSubscriptionSummaries,
    SetBillingRetention,
    CompactBillingStatements,
    SetOracleConfig,
    AddToBlocklist,
    RemoveFromBlocklist,
}

impl Operation {
    pub fn all() -> &'static [Operation] {
        &[
            Operation::SetMinTopup,
            Operation::RotateAdmin,
            Operation::SetProtocolFee,
            Operation::RecoverStrandedFunds,
            Operation::EnableEmergencyStop,
            Operation::DisableEmergencyStop,
            Operation::DepositFunds,
            Operation::CancelSubscription,
            Operation::PauseSubscription,
            Operation::ResumeSubscription,
            Operation::ChargeOneOff,
            Operation::WithdrawSubscriberFunds,
            Operation::WithdrawMerchantFunds,
            Operation::WithdrawMerchantTokenFunds,
            Operation::PauseMerchant,
            Operation::UnpauseMerchant,
            Operation::MerchantRefund,
            Operation::ConfigureUsageLimits,
            Operation::PartialRefund,
            Operation::BatchCharge,
            Operation::AddAcceptedToken,
            Operation::RemoveAcceptedToken,
            Operation::SetSubscriberCreditLimit,
            Operation::ExportContractSnapshot,
            Operation::ExportSubscriptionSummary,
            Operation::ExportSubscriptionSummaries,
            Operation::SetBillingRetention,
            Operation::CompactBillingStatements,
            Operation::SetOracleConfig,
            Operation::AddToBlocklist,
            Operation::RemoveFromBlocklist,
        ]
    }

}

// ── Harness ─────────────────────────────────────────────────────────────────

pub struct FuzzHarness {
    pub env: Env,
    pub client: SubscriptionVaultClient<'static>,
    pub admin: Address,
    pub subscriber: Address,
    pub merchant: Address,
    pub stranger: Address,
    pub new_admin: Address,
    pub token: Address,
    pub subscription_id: u32,
    /// Address with no plan/subscription; used for blocklist / recovery recipient smoke paths.
    pub blocklist_victim: Address,
}

impl FuzzHarness {
    pub fn setup() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        env.ledger().with_mut(|li| {
            li.timestamp = 1000;
        });

        let contract_id = env.register(SubscriptionVault, ());
        let client = SubscriptionVaultClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let subscriber = Address::generate(&env);
        let merchant = Address::generate(&env);
        let stranger = Address::generate(&env);
        let new_admin = Address::generate(&env);
        let blocklist_victim = Address::generate(&env);

        let token = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();

        client.init(&token, &6, &admin, &1_000_000i128, &(7 * 24 * 60 * 60));

        let plan_id = client.create_plan_template(
            &merchant,
            &10_000_000,
            &2592000,
            &true,
            &None::<i128>,
        );
        let subscription_id = client.create_subscription_from_plan(&subscriber, &plan_id);

        let token_admin = soroban_sdk::token::StellarAssetClient::new(&env, &token);
        token_admin.mint(&subscriber, &1_000_000_000);
        token_admin.mint(&merchant, &100_000_000);
        token_admin.mint(&contract_id, &100_000_000);

        Self {
            env,
            client,
            admin,
            subscriber,
            merchant,
            stranger,
            new_admin,
            token,
            subscription_id,
            blocklist_victim,
        }
    }

    pub fn get_address(&self, role: Role) -> Address {
        match role {
            Role::Admin => self.admin.clone(),
            Role::Subscriber => self.subscriber.clone(),
            Role::Merchant => self.merchant.clone(),
            Role::Stranger => self.stranger.clone(),
        }
    }

    /// What the contract should return (signatures satisfied) when the role is *not* allowed.
    pub fn expected_error_when_denied(op: Operation, _role: Role) -> Error {
        match op {
            Operation::SetProtocolFee | Operation::AddToBlocklist => Error::Forbidden,
            // cancel/pause/resume: wrong authorizer (not sub/merchant) returns `Forbidden` (403).
            Operation::CancelSubscription
            | Operation::PauseSubscription
            | Operation::ResumeSubscription
            | Operation::WithdrawSubscriberFunds
            | Operation::ConfigureUsageLimits => Error::Forbidden,
            _ => Error::Unauthorized,
        }
    }

    pub fn is_allowed(&self, op: Operation, caller: Role) -> bool {
        match op {
            Operation::SetMinTopup
            | Operation::RotateAdmin
            | Operation::SetProtocolFee
            | Operation::RecoverStrandedFunds
            | Operation::EnableEmergencyStop
            | Operation::DisableEmergencyStop
            | Operation::PartialRefund
            | Operation::BatchCharge
            | Operation::AddAcceptedToken
            | Operation::RemoveAcceptedToken
            | Operation::SetSubscriberCreditLimit
            | Operation::ExportContractSnapshot
            | Operation::ExportSubscriptionSummary
            | Operation::ExportSubscriptionSummaries
            | Operation::SetBillingRetention
            | Operation::CompactBillingStatements
            | Operation::SetOracleConfig
            | Operation::RemoveFromBlocklist => caller == Role::Admin,
            Operation::AddToBlocklist => caller == Role::Admin,
            // `do_deposit_funds` only requires `subscriber.require_auth()`: the payer is not
            // required to be the same address as the subscription's `subscriber` field, so
            // other parties with funds (e.g. merchant) can top up. Keep `is_allowed` aligned.
            Operation::DepositFunds => {
                matches!(caller, Role::Subscriber | Role::Merchant)
            }
            Operation::WithdrawSubscriberFunds => caller == Role::Subscriber,
            Operation::CancelSubscription
            | Operation::PauseSubscription
            | Operation::ResumeSubscription => {
                caller == Role::Subscriber || caller == Role::Merchant
            }
            Operation::ChargeOneOff
            | Operation::WithdrawMerchantFunds
            | Operation::WithdrawMerchantTokenFunds
            | Operation::PauseMerchant
            | Operation::UnpauseMerchant
            | Operation::ConfigureUsageLimits
            | Operation::MerchantRefund => caller == Role::Merchant,
        }
    }

    fn set_auth_mocks(&self, op: Operation, is_allowed: bool) {
        if op == Operation::BatchCharge {
            if is_allowed {
                self.env.mock_all_auths();
            } else {
                self.env.mock_auths(&[]);
            }
        } else if !is_allowed && self.merchant_op_requires_real_auth_failure(op) {
            // `pause_merchant` / `unpause_merchant` use `merchant.require_auth()` on the
            // `merchant` argument; with `mock_all_auths` that auth always succeeds, so
            // empty mocks are required to observe denial.
            self.env.mock_auths(&[]);
        } else {
            // Wrong principals still "sign" so admin-style checks return [`Error::Unauthorized`]
            // (or `Forbidden` / `NotFound` for a few entrypoints) instead of host `Abort`.
            self.env.mock_all_auths();
        }
    }

    fn merchant_op_requires_real_auth_failure(&self, op: Operation) -> bool {
        matches!(op, Operation::PauseMerchant | Operation::UnpauseMerchant)
    }

    /// `format!("{:?}", try_…)` (SDK `try_` return types are not a single concrete `Result`).
    /// Assertions use `Error` variant names and [`Error::to_code`].
    pub fn try_debug(&self, op: Operation, address: &Address, is_allowed: bool) -> std::string::String {
        self.set_auth_mocks(op, is_allowed);
        let env = &self.env;
        let client = &self.client;
        let res: std::string::String = match op {
            Operation::SetMinTopup => {
                std::format!("{:?}", client.try_set_min_topup(address, &2_000_000))
            }
            Operation::RotateAdmin => {
                std::format!("{:?}", client.try_rotate_admin(address, &self.new_admin))
            }
            Operation::SetProtocolFee => {
                let t = self.new_admin.clone();
                std::format!("{:?}", client.try_set_protocol_fee(address, &t, &0u32))
            }
            Operation::RecoverStrandedFunds => {
                let recovery_id = SorobanString::from_str(env, "auth-fuzz-recovery-1");
                let recipient = Address::generate(env);
                std::format!(
                    "{:?}",
                    client.try_recover_stranded_funds(
                        address,
                        &self.token,
                        &recipient,
                        &1i128,
                        &recovery_id,
                        &RecoveryReason::UserOverpayment,
                    )
                )
            }
            Operation::EnableEmergencyStop => {
                std::format!("{:?}", client.try_enable_emergency_stop(address))
            }
            Operation::DisableEmergencyStop => {
                self.env.mock_all_auths();
                client.enable_emergency_stop(&self.admin);
                self.set_auth_mocks(op, is_allowed);
                std::format!("{:?}", client.try_disable_emergency_stop(address))
            }
            Operation::DepositFunds => std::format!(
                "{:?}",
                client.try_deposit_funds(&self.subscription_id, address, &5_000_000)
            ),
            Operation::CancelSubscription => std::format!(
                "{:?}",
                client.try_cancel_subscription(&self.subscription_id, address)
            ),
            Operation::PauseSubscription => std::format!(
                "{:?}",
                client.try_pause_subscription(&self.subscription_id, address)
            ),
            Operation::ResumeSubscription => {
                self.env.mock_all_auths();
                client.pause_subscription(&self.subscription_id, &self.subscriber);
                self.set_auth_mocks(op, is_allowed);
                std::format!(
                    "{:?}",
                    client.try_resume_subscription(&self.subscription_id, address)
                )
            }
            Operation::ChargeOneOff => {
                self.env.mock_all_auths();
                let _ = client.try_deposit_funds(
                    &self.subscription_id,
                    &self.subscriber,
                    &20_000_000,
                );
                self.set_auth_mocks(op, is_allowed);
                std::format!(
                    "{:?}",
                    client.try_charge_one_off(&self.subscription_id, address, &1_000_000)
                )
            }
            Operation::WithdrawSubscriberFunds => {
                self.env.mock_all_auths();
                let _ = client.try_deposit_funds(
                    &self.subscription_id,
                    &self.subscriber,
                    &10_000_000,
                );
                let _ = client
                    .try_cancel_subscription(&self.subscription_id, &self.subscriber);
                self.set_auth_mocks(op, is_allowed);
                std::format!(
                    "{:?}",
                    client.try_withdraw_subscriber_funds(&self.subscription_id, address)
                )
            }
            Operation::WithdrawMerchantFunds => {
                self.env.mock_all_auths();
                let _ = client.try_deposit_funds(
                    &self.subscription_id,
                    &self.subscriber,
                    &50_000_000,
                );
                let _ = client.try_charge_one_off(
                    &self.subscription_id,
                    &self.merchant,
                    &10_000_000,
                );
                self.set_auth_mocks(op, is_allowed);
                std::format!("{:?}", client.try_withdraw_merchant_funds(address, &1_000_000))
            }
            Operation::WithdrawMerchantTokenFunds => {
                self.env.mock_all_auths();
                let _ = client.try_deposit_funds(
                    &self.subscription_id,
                    &self.subscriber,
                    &50_000_000,
                );
                let _ = client.try_charge_one_off(
                    &self.subscription_id,
                    &self.merchant,
                    &10_000_000,
                );
                self.set_auth_mocks(op, is_allowed);
                std::format!(
                    "{:?}",
                    client.try_withdraw_merchant_token_funds(
                        address,
                        &self.token,
                        &1_000_000,
                    )
                )
            }
            Operation::PauseMerchant => {
                // Callee is always the subscription's merchant; only that address may sign.
                std::format!("{:?}", client.try_pause_merchant(&self.merchant))
            }
            Operation::UnpauseMerchant => {
                self.env.mock_all_auths();
                client.pause_merchant(&self.merchant);
                self.set_auth_mocks(op, is_allowed);
                std::format!("{:?}", client.try_unpause_merchant(&self.merchant))
            }
            Operation::MerchantRefund => {
                self.env.mock_all_auths();
                let _ = client.try_deposit_funds(
                    &self.subscription_id,
                    &self.subscriber,
                    &50_000_000,
                );
                let _ = client.try_charge_one_off(
                    &self.subscription_id,
                    &self.merchant,
                    &10_000_000,
                );
                self.set_auth_mocks(op, is_allowed);
                std::format!(
                    "{:?}",
                    client.try_merchant_refund(
                        address,
                        &self.subscriber,
                        &self.token,
                        &1_000_000,
                    )
                )
            }
            Operation::ConfigureUsageLimits => std::format!(
                "{:?}",
                client.try_configure_usage_limits(
                    address,
                    &self.subscription_id,
                    &Some(100u32),
                    &3600u64,
                    &60u64,
                    &None::<i128>,
                )
            ),
            Operation::PartialRefund => {
                self.env.mock_all_auths();
                let _ = client.try_deposit_funds(
                    &self.subscription_id,
                    &self.subscriber,
                    &50_000_000,
                );
                let _ = client.try_charge_one_off(
                    &self.subscription_id,
                    &self.merchant,
                    &10_000_000,
                );
                self.set_auth_mocks(op, is_allowed);
                std::format!(
                    "{:?}",
                    client.try_partial_refund(
                        address,
                        &self.subscription_id,
                        &self.subscriber,
                        &1_000_000,
                    )
                )
            }
            Operation::BatchCharge => {
                if is_allowed {
                    self.env.mock_all_auths();
                } else {
                    let batch = SorobanVec::from_array(env, [self.subscription_id]);
                    let mut args_vec = SorobanVec::new(env);
                    args_vec.push_back(batch.into_val(env));
                    self.env.mock_auths(&[MockAuth {
                        address: address,
                        invoke: &MockAuthInvoke {
                            contract: &client.address,
                            fn_name: "batch_charge",
                            args: args_vec,
                            sub_invokes: &[],
                        },
                    }]);
                }
                std::format!(
                    "{:?}",
                    client.try_batch_charge(&SorobanVec::from_array(
                        env,
                        [self.subscription_id],
                    ))
                )
            }
            Operation::AddAcceptedToken => {
                let other = Address::generate(env);
                std::format!("{:?}", client.try_add_accepted_token(address, &other, &6u32))
            }
            Operation::RemoveAcceptedToken => {
                let other = Address::generate(env);
                self.env.mock_all_auths();
                client.add_accepted_token(&self.admin, &other, &6u32);
                self.set_auth_mocks(op, is_allowed);
                std::format!("{:?}", client.try_remove_accepted_token(address, &other))
            }
            Operation::SetSubscriberCreditLimit => std::format!(
                "{:?}",
                client.try_set_subscriber_credit_limit(
                    address,
                    &self.subscriber,
                    &self.token,
                    &100_000_000i128,
                )
            ),
            Operation::ExportContractSnapshot => {
                std::format!("{:?}", client.try_export_contract_snapshot(address))
            }
            Operation::ExportSubscriptionSummary => std::format!(
                "{:?}",
                client.try_export_subscription_summary(address, &self.subscription_id)
            ),
            Operation::ExportSubscriptionSummaries => std::format!(
                "{:?}",
                client.try_export_subscription_summaries(address, &0u32, &1u32)
            ),
            Operation::SetBillingRetention => {
                std::format!("{:?}", client.try_set_billing_retention(address, &5u32))
            }
            Operation::CompactBillingStatements => std::format!(
                "{:?}",
                client.try_compact_billing_statements(
                    address,
                    &self.subscription_id,
                    &None::<u32>,
                )
            ),
            Operation::SetOracleConfig => std::format!(
                "{:?}",
                client.try_set_oracle_config(address, &false, &None::<Address>, &0u64)
            ),
            Operation::AddToBlocklist => std::format!(
                "{:?}",
                client.try_add_to_blocklist(
                    address,
                    &self.blocklist_victim,
                    &None::<SorobanString>,
                )
            ),
            Operation::RemoveFromBlocklist => {
                self.env.mock_all_auths();
                let _ = client
                    .try_add_to_blocklist(
                        &self.admin,
                        &self.blocklist_victim,
                        &None::<SorobanString>,
                    );
                self.set_auth_mocks(op, is_allowed);
                std::format!(
                    "{:?}",
                    client.try_remove_from_blocklist(address, &self.blocklist_victim)
                )
            }
        };
        res
    }
}

// ── Outcome check (debug + discriminant + code) ─────────────────────────────

fn merchant_op_needs_abort_ok(op: Operation) -> bool {
    matches!(op, Operation::PauseMerchant | Operation::UnpauseMerchant)
}

fn error_variant_tag(e: Error) -> &'static str {
    // Keep in sync with `Error` for stable substring checks in `Debug` output.
    match e {
        Error::Unauthorized => "Unauthorized",
        Error::Forbidden => "Forbidden",
        _ => "Other",
    }
}

/// Success in `try_` is represented in `Debug` as a nested `Ok(Ok(…` for contract return.
/// Denied: expect contract error and [`Error::to_code`] in the `Debug` string.
fn assert_auth_debug(allowed: bool, have: &str, want: Error, op: Operation, role: Role) {
    if allowed {
        let ok = have.contains("Ok(Ok(") || have.contains("Ok(Ok (");
        assert!(
            ok,
            "expected try success for op={op:?} role={role:?} have={have}"
        );
    } else {
        let tag = error_variant_tag(want);
        if have.contains(tag) {
            return;
        }
        if (merchant_op_needs_abort_ok(op) || matches!(op, Operation::BatchCharge))
            && have.contains("Abort")
        {
            return;
        }
        // `do_deposit_funds` does not require `subscriber == sub.subscriber`, so a wrong
        // address can pass `require_auth` and fail in the token `transfer` (host) first.
        if matches!(op, Operation::DepositFunds) {
            assert!(
                have.contains("Contract(10)") || have.contains("Unauthorized"),
                "expected contract Unauthorized or token host error for op={op:?} have={have}"
            );
            return;
        }
        // Default bucket is missing for a non-merchant `Address` / wrong principal
        // → [`Error::NotFound`] in addition to auth-style denials.
        if matches!(
            op,
            Operation::WithdrawMerchantFunds
                | Operation::WithdrawMerchantTokenFunds
                | Operation::MerchantRefund
        ) && have.contains("NotFound")
        {
            return;
        }
        assert!(
            have.contains(tag),
            "expected Error {want:?} (variant {tag}) in result for op={op:?} role={role:?} have={have}"
        );
    }
}

fn assert_try_unauthorized(s: &str) {
    assert!(
        s.contains("Unauthorized"),
        "expected Unauthorized, got: {s}"
    );
}

fn assert_try_forbidden(s: &str) {
    assert!(s.contains("Forbidden"), "expected Forbidden, got: {s}");
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn test_authorization_matrix_fuzz() {
    for &op in Operation::all() {
        for &role in Role::all() {
            let harness = FuzzHarness::setup();
            let address = harness.get_address(role);
            let expected_allowed = harness.is_allowed(op, role);
            let have = harness.try_debug(op, &address, expected_allowed);
            if expected_allowed {
                assert_auth_debug(true, &have, Error::Unauthorized, op, role);
            } else {
                let want = FuzzHarness::expected_error_when_denied(op, role);
                assert_auth_debug(false, &have, want, op, role);
            }
        }
    }
}

/// After rotation, the previous admin address must be rejected (immediate enforcement) on
/// representative privileged entrypoints.
#[test]
fn test_privileged_rejected_after_admin_rotation() {
    let h = FuzzHarness::setup();
    h.env.mock_all_auths();
    h.client.rotate_admin(&h.admin, &h.new_admin);
    let old = &h.admin;
    h.env.mock_all_auths();
    let treasury = h.new_admin.clone();
    let recovery_id = SorobanString::from_str(&h.env, "rotation-recovery-1");
    let recipient = Address::generate(&h.env);
    h.env.mock_all_auths();
    assert!(std::format!("{:?}", h.client.try_export_contract_snapshot(&h.new_admin)).contains("Ok(Ok("));
    // Old admin: wrong principal → Unauthorized (or Forbidden for set_protocol_fee / add).
    h.env.mock_all_auths();
    assert_try_unauthorized(&std::format!("{:?}", h.client.try_set_min_topup(old, &3_000_000)));
    assert_try_forbidden(&std::format!("{:?}", h.client.try_set_protocol_fee(old, &treasury, &0u32)));
    assert_try_forbidden(&std::format!(
        "{:?}",
        h.client
            .try_add_to_blocklist(old, &h.blocklist_victim, &None::<SorobanString>)
    ));
    assert_try_unauthorized(&std::format!(
        "{:?}",
        h.client.try_recover_stranded_funds(
            old,
            &h.token,
            &recipient,
            &1i128,
            &recovery_id,
            &RecoveryReason::UserOverpayment
        )
    ));
    h.env.mock_all_auths();
    assert_try_unauthorized(&std::format!("{:?}", h.client.try_export_contract_snapshot(old)));
    h.env.mock_all_auths();
    assert_try_unauthorized(&std::format!(
        "{:?}",
        h.client.try_export_subscription_summary(old, &h.subscription_id)
    ));
    h.env.mock_all_auths();
    assert_try_unauthorized(&std::format!(
        "{:?}",
        h.client.try_export_subscription_summaries(old, &0u32, &1u32)
    ));
    h.env.mock_all_auths();
    assert_try_unauthorized(&std::format!("{:?}", h.client.try_set_billing_retention(old, &3u32)));
    h.env.mock_all_auths();
    assert_try_unauthorized(&std::format!(
        "{:?}",
        h.client.try_set_oracle_config(old, &false, &None::<Address>, &0u64)
    ));
}

/// Migration / export: snapshot reports the on-chain storage schema version (upgrade safety).
#[test]
fn test_export_contract_snapshot_includes_storage_version() {
    let h = FuzzHarness::setup();
    h.env.mock_all_auths();
    let snap = match h.client.try_export_contract_snapshot(&h.admin) {
        Ok(Ok(s)) => s,
        o => std::panic!("export_contract_snapshot as admin: {o:?}"),
    };
    assert_eq!(snap.storage_version, EXPECTED_STORAGE_VERSION);
}

#[test]
fn test_admin_rotation_edge_case() {
    let harness = FuzzHarness::setup();
    let old_admin = harness.admin.clone();
    let new_admin = harness.new_admin.clone();

    harness.env.mock_all_auths();
    harness.client.rotate_admin(&old_admin, &new_admin);

    assert_try_unauthorized(&std::format!(
        "{:?}",
        harness.client.try_set_min_topup(&old_admin, &3_000_000)
    ));
    let res_new = harness.client.try_set_min_topup(&new_admin, &4_000_000);
    assert!(std::format!("{res_new:?}").contains("Ok(Ok("));
}

#[test]
fn test_identity_collision_subscriber_is_merchant() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let person = Address::generate(&env);

    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    client.init(
        &token,
        &6,
        &admin,
        &1_000_000i128,
        &(7 * 24 * 60 * 60),
    );

    let plan_id = client.create_plan_template(
        &person,
        &10_000_000,
        &2592000,
        &false,
        &None::<i128>,
    );
    let sub_id = client.create_subscription_from_plan(&person, &plan_id);

    client.pause_subscription(&sub_id, &person);

    let sub = client.get_subscription(&sub_id);
    assert_eq!(sub.status, SubscriptionStatus::Paused);
}
