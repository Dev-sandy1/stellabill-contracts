# Subscription Vault Event Schema — Complete Reference

This document describes the canonical event schemas emitted by the `subscription_vault` contract for indexing and monitoring all subscription lifecycle actions, fund movements, and administrative operations.

## Event Design Principles

1. **Stable Schema**: All events have fixed field names and types for reliable indexing
2. **Complete Context**: Events include all necessary fields to reconstruct state without additional queries
3. **Security**: Events never leak optional sensitive metadata values
4. **Determinism**: Batch operations emit events in deterministic order
5. **Failure Safety**: Failed operations never emit success events

## Core Lifecycle Events

### SubscriptionCreatedEvent

**Topic:** `created`

Emitted when a new subscription is created (via `create_subscription`, `create_subscription_with_token`, or `create_subscription_from_plan`).

**Fields:**
- `subscription_id` (u32): Unique identifier for the subscription
- `subscriber` (Address): Address of the subscriber
- `merchant` (Address): Address of the merchant receiving payments
- `token` (Address): Settlement token used for all charges
- `amount` (i128): Payment amount per billing interval (in token base units)
- `interval_seconds` (u64): Billing interval in seconds
- `lifetime_cap` (Option<i128>): Optional maximum total charges over subscription lifetime
- `expires_at` (Option<u64>): Optional expiration timestamp
- `timestamp` (u64): Ledger timestamp when subscription was created

**Indexing Strategy:**
- Index by `subscription_id` for direct lookup
- Index by `subscriber` and `merchant` for filtering user/merchant subscriptions
- Index by `token` for per-token analytics
- Track creation timestamp for cohort analysis

**Example Use Cases:**
- Build subscriber dashboard showing all subscriptions
- Merchant analytics on new subscriptions by token
- Monitor subscription creation rate and lifetime cap distribution

---

### FundsDepositedEvent

**Topic:** `deposited`

Emitted when a subscriber deposits funds to their subscription vault.

**Fields:**
- `subscription_id` (u32): Subscription receiving the deposit
- `subscriber` (Address): Address making the deposit
- `token` (Address): Token deposited
- `amount` (i128): Amount deposited (in token base units)
- `new_balance` (i128): Total prepaid balance after deposit
- `timestamp` (u64): Ledger timestamp when deposit was processed

**Indexing Strategy:**
- Index by `subscription_id` to track balance history
- Aggregate deposits per subscriber for analytics
- Monitor `new_balance` for low-balance alerts

**Example Use Cases:**
- Display deposit history in subscriber UI
- Alert subscribers when balance is low
- Track total value locked in the contract per token

---

### SubscriptionChargedEvent

**Topic:** `charged`

Emitted when a subscription is successfully charged for a billing interval.

**Fields:**
- `subscription_id` (u32): Subscription that was charged
- `subscriber` (Address): Subscription owner
- `merchant` (Address): Merchant receiving the payment
- `token` (Address): Token charged
- `amount` (i128): Amount charged in this interval (gross amount before fees)
- `lifetime_charged` (i128): Cumulative total charged over subscription lifetime
- `remaining_balance` (i128): Prepaid balance remaining after this charge
- `timestamp` (u64): Ledger timestamp when charge was processed

**Indexing Strategy:**
- Index by `subscription_id` for payment history
- Index by `merchant` to track merchant revenue
- Monitor `remaining_balance` for insufficient balance warnings
- Track `lifetime_charged` for lifetime cap enforcement

**Example Use Cases:**
- Generate merchant revenue reports
- Track subscription payment history
- Trigger notifications when balance is insufficient for next charge
- Monitor lifetime cap progress

---

### SubscriptionChargeFailedEvent

**Topic:** `charge_failed`

Emitted when a subscription charge attempt fails due to insufficient balance.

**Fields:**
- `subscription_id` (u32): Subscription that failed to charge
- `merchant` (Address): Merchant who would have received payment
- `required_amount` (i128): Amount required for the charge
- `available_balance` (i128): Current prepaid balance
- `shortfall` (i128): Difference between required and available
- `resulting_status` (SubscriptionStatus): New status after failure (GracePeriod or InsufficientBalance)
- `timestamp` (u64): Ledger timestamp when charge failed

**Indexing Strategy:**
- Index by `subscription_id` to track failure history
- Monitor `shortfall` to estimate required top-up
- Track `resulting_status` transitions

**Example Use Cases:**
- Alert subscribers to top up their balance
- Track grace period usage
- Merchant analytics on failed charges

---

### SubscriptionPausedEvent

**Topic:** `sub_paused`

Emitted when a subscription is paused (no charges until resumed).

**Fields:**
- `subscription_id` (u32): Subscription that was paused
- `subscriber` (Address): Subscription owner
- `merchant` (Address): Merchant
- `authorizer` (Address): Address that authorized the pause (subscriber or merchant)
- `timestamp` (u64): Ledger timestamp when paused

**Indexing Strategy:**
- Index by `subscription_id` to track status changes
- Track pause duration by comparing with resume events
- Analyze pause patterns by authorizer

**Example Use Cases:**
- Display paused status in UI
- Analytics on pause frequency and duration
- Notify relevant parties of status change

---

### SubscriptionResumedEvent

**Topic:** `sub_resumed`

Emitted when a paused subscription is resumed.

**Fields:**
- `subscription_id` (u32): Subscription that was resumed
- `subscriber` (Address): Subscription owner
- `merchant` (Address): Merchant
- `authorizer` (Address): Address that authorized the resume (subscriber or merchant)
- `timestamp` (u64): Ledger timestamp when resumed

**Indexing Strategy:**
- Index by `subscription_id` to track status changes
- Calculate pause duration by comparing with pause events
- Analyze resume patterns

**Example Use Cases:**
- Update subscription status in UI
- Track subscription lifecycle metrics
- Resume billing operations

---

### SubscriptionCancelledEvent

**Topic:** `subscription_cancelled`

Emitted when a subscription is cancelled by subscriber or merchant.

**Fields:**
- `subscription_id` (u32): Subscription that was cancelled
- `subscriber` (Address): Subscription owner
- `merchant` (Address): Merchant
- `token` (Address): Settlement token
- `authorizer` (Address): Address that authorized the cancellation
- `refund_amount` (i128): Remaining prepaid balance available for subscriber withdrawal
- `timestamp` (u64): Ledger timestamp when cancelled

**Indexing Strategy:**
- Index by `subscription_id` for final status
- Track cancellation rate by subscriber/merchant
- Monitor `refund_amount` for refund processing

**Example Use Cases:**
- Process refunds to subscribers
- Calculate churn rate and cancellation analytics
- Archive cancelled subscriptions

---

### SubscriptionExpiredEvent

**Topic:** `subscription_expired`

Emitted when a subscription automatically expires based on its `expires_at` timestamp.

**Fields:**
- `subscription_id` (u32): Subscription that expired
- `timestamp` (u64): Ledger timestamp when expiration was detected

**Indexing Strategy:**
- Index by `subscription_id` for lifecycle tracking
- Monitor expiration patterns

**Example Use Cases:**
- Notify subscribers of expiration
- Track subscription lifecycle completion
- Trigger cleanup workflows

---

### SubscriptionArchivedEvent

**Topic:** `subscription_archived`

Emitted when a subscription is archived (reduced storage, read-only).

**Fields:**
- `subscription_id` (u32): Subscription that was archived
- `timestamp` (u64): Ledger timestamp when archived

**Indexing Strategy:**
- Index by `subscription_id` for lifecycle tracking
- Track archived subscriptions for cleanup

**Example Use Cases:**
- Optimize storage by archiving old subscriptions
- Track subscription lifecycle completion

---

### SubscriptionRecoveryReadyEvent

**Topic:** `recovery_ready`

Emitted after a deposit when a previously underfunded subscription is ready to be resumed.

**Fields:**
- `subscription_id` (u32): Subscription that is ready for recovery
- `subscriber` (Address): Subscription owner
- `prepaid_balance` (i128): Current prepaid balance after deposit
- `required_amount` (i128): Amount required for next charge
- `timestamp` (u64): Ledger timestamp when recovery became possible

**Indexing Strategy:**
- Index by `subscription_id` to track recovery events
- Monitor recovery patterns

**Example Use Cases:**
- Notify subscribers that their subscription can be resumed
- Track recovery success rates

---

## Fund Movement Events

### MerchantWithdrawalEvent

**Topic:** `withdrawn`

Emitted when a merchant withdraws accumulated funds.

**Fields:**
- `merchant` (Address): Merchant withdrawing funds
- `token` (Address): Token bucket debited for the withdrawal
- `amount` (i128): Amount withdrawn (in token base units)
- `remaining_balance` (i128): Merchant's accumulated balance remaining after withdrawal
- `timestamp` (u64): Ledger timestamp when withdrawal was processed

**Indexing Strategy:**
- Index by `merchant` to track withdrawal history
- Aggregate total withdrawals per merchant
- Monitor withdrawal frequency

**Example Use Cases:**
- Display merchant withdrawal history
- Track merchant payout schedules
- Reconcile merchant balances

---

### SubscriberWithdrawalEvent

**Topic:** `sub_withdrawn`

Emitted when a subscriber withdraws funds after cancellation.

**Fields:**
- `subscription_id` (u32): Subscription from which funds were withdrawn
- `subscriber` (Address): Subscriber receiving the refund
- `token` (Address): Token withdrawn
- `amount` (i128): Amount withdrawn (in token base units)
- `timestamp` (u64): Ledger timestamp when withdrawal was processed

**Indexing Strategy:**
- Index by `subscription_id` to track refund history
- Monitor refund patterns

**Example Use Cases:**
- Track subscriber refunds
- Reconcile subscription balances

---

### PartialRefundEvent

**Topic:** `partial_refund`

Emitted when an admin processes a partial refund for a subscription.

**Fields:**
- `subscription_id` (u32): Subscription receiving the refund
- `subscriber` (Address): Subscriber who receives the refunded amount
- `token` (Address): Token refunded
- `amount` (i128): Amount refunded in token base units
- `timestamp` (u64): Ledger timestamp when refund was processed

**Indexing Strategy:**
- Index by `subscription_id` to track refund history
- Monitor admin refund activity

**Example Use Cases:**
- Track admin-initiated refunds
- Audit refund operations

---

### MerchantRefundEvent

**Topic:** `merchant_refund`

Emitted when a merchant issues a refund to a subscriber.

**Fields:**
- `merchant` (Address): Merchant issuing the refund
- `subscriber` (Address): Subscriber receiving the refund
- `token` (Address): Token refunded
- `amount` (i128): Amount refunded
- `timestamp` (u64): Ledger timestamp when refund was processed

**Indexing Strategy:**
- Index by `merchant` and `subscriber` to track refund history
- Monitor merchant refund patterns

**Example Use Cases:**
- Track merchant-initiated refunds
- Customer service analytics

---

### OneOffChargedEvent

**Topic:** `oneoff_ch`

Emitted when a merchant-initiated one-off charge is applied.

**Fields:**
- `subscription_id` (u32): Subscription charged
- `subscriber` (Address): Subscription owner
- `merchant` (Address): Merchant receiving the payment
- `token` (Address): Token charged
- `amount` (i128): Amount charged
- `remaining_balance` (i128): Prepaid balance remaining after charge
- `timestamp` (u64): Ledger timestamp when charge was processed

**Indexing Strategy:**
- Index by `subscription_id` to track one-off charges
- Monitor merchant one-off charge patterns

**Example Use Cases:**
- Track merchant-initiated charges
- Reconcile subscription balances

---

### UsageStatementEvent

**Topic:** `usage_charged`

Emitted when a usage-based charge is applied.

**Fields:**
- `subscription_id` (u32): Subscription charged
- `merchant` (Address): Merchant receiving the payment
- `usage_amount` (i128): Amount charged for usage
- `token` (Address): Token charged
- `timestamp` (u64): Ledger timestamp when charge was processed
- `reference` (String): Idempotency reference for this usage charge

**Indexing Strategy:**
- Index by `subscription_id` to track usage charges
- Index by `reference` for idempotency verification

**Example Use Cases:**
- Track usage-based billing
- Reconcile metered usage

---

## Administrative Events

### AdminRotatedEvent

**Topic:** `admin_rotated`

Emitted when the contract admin is rotated to a new address.

**Fields:**
- `old_admin` (Address): The admin address that initiated the rotation (now revoked)
- `new_admin` (Address): The new admin address that received privileges
- `timestamp` (u64): Ledger timestamp when the rotation occurred

**Indexing Strategy:**
- Index by `old_admin` and `new_admin` for audit trails
- Track timestamp for rotation history
- Maintain current admin state for authorization checks

**Example Use Cases:**
- Build admin rotation audit log
- Alert on admin changes for security monitoring
- Update off-chain systems with current admin address

---

### RecoveryEvent

**Topic:** `recovery`

Emitted when the admin recovers stranded funds from the contract.

**Fields:**
- `admin` (Address): The admin who authorized the recovery
- `recipient` (Address): The destination address receiving the recovered funds
- `token` (Address): Token recovered
- `amount` (i128): Amount recovered (in token base units)
- `reason` (RecoveryReason): Enum—`UserOverpayment` (0), `FailedTransfer` (1), `ExpiredEscrow` (2), `SystemCorrection` (3)
- `timestamp` (u64): Ledger timestamp when recovery was executed

**Indexing Strategy:**
- Index by `admin` to track recovery actions per admin
- Index by `recipient` for recipient-side history
- Aggregate amounts by reason for analytics

**Example Use Cases:**
- Audit trail for fund recoveries
- Monitor admin recovery activity
- Analytics on recovery reasons and amounts

---

### EmergencyStopEnabledEvent

**Topic:** `emergency_stop_enabled`

Emitted when the emergency stop circuit breaker is activated.

**Fields:**
- `admin` (Address): Admin who enabled the emergency stop
- `timestamp` (u64): Ledger timestamp when enabled

**Indexing Strategy:**
- Track emergency stop activations for security monitoring

**Example Use Cases:**
- Alert on emergency stop activation
- Track circuit breaker usage

---

### EmergencyStopDisabledEvent

**Topic:** `emergency_stop_disabled`

Emitted when the emergency stop circuit breaker is deactivated.

**Fields:**
- `admin` (Address): Admin who disabled the emergency stop
- `timestamp` (u64): Ledger timestamp when disabled

**Indexing Strategy:**
- Track emergency stop deactivations

**Example Use Cases:**
- Alert on emergency stop deactivation
- Track circuit breaker usage

---

## Plan Template Events

### PlanTemplateCreatedEvent

**Topic:** `plan_created`

Emitted when a new plan template is created.

**Fields:**
- `plan_template_id` (u32): Unique identifier for the plan template
- `merchant` (Address): Merchant who owns this plan
- `token` (Address): Settlement token for subscriptions created from this plan
- `amount` (i128): Billing amount per interval
- `interval_seconds` (u64): Billing interval duration
- `usage_enabled` (bool): Whether usage-based charging is allowed
- `lifetime_cap` (Option<i128>): Optional lifetime cap for subscriptions
- `timestamp` (u64): Ledger timestamp when plan was created

**Indexing Strategy:**
- Index by `plan_template_id` for direct lookup
- Index by `merchant` for merchant plan listings

**Example Use Cases:**
- Track merchant plan offerings
- Display available plans to subscribers

---

### PlanTemplateUpdatedEvent

**Topic:** `plan_template_updated`

Emitted when a plan template is updated to a new version.

**Fields:**
- `template_key` (u32): Logical template group identifier shared by all versions
- `old_plan_id` (u32): Previous plan template ID
- `new_plan_id` (u32): Newly created plan template ID representing the updated version
- `version` (u32): Version number of the new plan template
- `merchant` (Address): Merchant that owns this plan template
- `timestamp` (u64): Ledger timestamp when the update occurred

**Indexing Strategy:**
- Index by `template_key` to track version history
- Track version progression

**Example Use Cases:**
- Track plan template versioning
- Notify subscribers of plan updates

---

### PlanMaxActiveUpdatedEvent

**Topic:** `plan_max_active_set`

Emitted when a plan's max-active-subscriptions limit is configured.

**Fields:**
- `plan_template_id` (u32): Plan template whose limit was changed
- `merchant` (Address): Merchant that owns the plan and authorized the change
- `max_active` (u32): New limit value (`0` = unlimited)
- `timestamp` (u64): Ledger timestamp when the change was applied

**Indexing Strategy:**
- Index by `plan_template_id` to track limit changes

**Example Use Cases:**
- Track plan concurrency limits
- Monitor plan capacity

---

### SubscriptionMigratedEvent

**Topic:** `subscription_migrated`

Emitted when a subscription is migrated from one plan template version to another.

**Fields:**
- `subscription_id` (u32): Subscription that was migrated
- `template_key` (u32): Logical template group identifier
- `from_plan_id` (u32): Plan template ID the subscription was previously pinned to
- `to_plan_id` (u32): Plan template ID the subscription is now pinned to
- `merchant` (Address): Merchant that owns the plan templates
- `subscriber` (Address): Subscriber that authorized the migration
- `timestamp` (u64): Ledger timestamp when the migration occurred

**Indexing Strategy:**
- Index by `subscription_id` to track migration history
- Track migration patterns

**Example Use Cases:**
- Track subscription migrations
- Monitor plan version adoption

---

## Merchant Events

### MerchantPausedEvent

**Topic:** `merchant_paused`

Emitted when a merchant enables their blanket pause.

**Fields:**
- `merchant` (Address): Merchant that was paused
- `timestamp` (u64): Ledger timestamp when paused

**Indexing Strategy:**
- Index by `merchant` to track pause state

**Example Use Cases:**
- Track merchant pause state
- Monitor merchant activity

---

### MerchantUnpausedEvent

**Topic:** `merchant_unpaused`

Emitted when a merchant disables their blanket pause.

**Fields:**
- `merchant` (Address): Merchant that was unpaused
- `timestamp` (u64): Ledger timestamp when unpaused

**Indexing Strategy:**
- Index by `merchant` to track pause state

**Example Use Cases:**
- Track merchant pause state
- Monitor merchant activity

---

## Protocol Fee Events

### ProtocolFeeChargedEvent

**Topic:** `protocol_fee_charged`

Emitted when a protocol fee is charged on a subscription interval.

**Fields:**
- `subscription_id` (u32): Subscription that was charged
- `treasury` (Address): Treasury address receiving the fee
- `fee_amount` (i128): Fee amount routed to treasury
- `merchant_amount` (i128): Net amount credited to merchant after fee deduction
- `timestamp` (u64): Ledger timestamp when fee was charged

**Indexing Strategy:**
- Index by `subscription_id` to track fee history
- Aggregate fees by treasury

**Example Use Cases:**
- Track protocol fee revenue
- Reconcile merchant and treasury balances

---

### ProtocolFeeConfiguredEvent

**Topic:** `protocol_fee_configured`

Emitted when the protocol fee configuration is updated.

**Fields:**
- `admin` (Address): Admin who configured the fee
- `treasury` (Address): Treasury address receiving fees
- `fee_bps` (u32): Fee in basis points (0–10,000)
- `timestamp` (u64): Ledger timestamp when configured

**Indexing Strategy:**
- Track fee configuration changes

**Example Use Cases:**
- Audit fee configuration changes
- Monitor protocol fee settings

---

## Lifetime Cap Events

### LifetimeCapReachedEvent

**Topic:** `lifetime_cap_reached`

Emitted when the lifetime charge cap is reached and the subscription is auto-cancelled.

**Fields:**
- `subscription_id` (u32): Subscription that reached its cap
- `lifetime_cap` (i128): The configured lifetime cap that was reached
- `lifetime_charged` (i128): Total charged at the point the cap was reached
- `timestamp` (u64): Ledger timestamp when the cap was reached

**Indexing Strategy:**
- Index by `subscription_id` to track cap events
- Monitor cap exhaustion patterns

**Example Use Cases:**
- Notify subscribers of cap exhaustion
- Track subscription lifecycle completion
- Merchant analytics on capped subscriptions

---

## Metadata Events

### MetadataSetEvent

**Topic:** `metadata_set`

Emitted when metadata is set or updated on a subscription.

**Fields:**
- `subscription_id` (u32): Subscription whose metadata was updated
- `key` (String): Metadata key that was set
- `authorizer` (Address): Address that authorized the change (subscriber or merchant)

**Note:** The metadata value is NOT included in the event for security reasons.

**Indexing Strategy:**
- Index by `subscription_id` to track metadata changes
- Track metadata key usage

**Example Use Cases:**
- Audit metadata changes
- Track metadata key usage patterns

---

### MetadataDeletedEvent

**Topic:** `metadata_deleted`

Emitted when metadata is deleted from a subscription.

**Fields:**
- `subscription_id` (u32): Subscription whose metadata was deleted
- `key` (String): Metadata key that was deleted
- `authorizer` (Address): Address that authorized the deletion

**Indexing Strategy:**
- Index by `subscription_id` to track metadata changes

**Example Use Cases:**
- Audit metadata deletions

---

## Blocklist Events

### BlocklistAddedEvent

**Topic:** `blocklist_added`

Emitted when a subscriber is added to the blocklist.

**Fields:**
- `subscriber` (Address): Subscriber that was blocklisted
- `added_by` (Address): Address that authorized the blocklist addition (admin or merchant)

**Indexing Strategy:**
- Index by `subscriber` to track blocklist status

**Example Use Cases:**
- Track blocklist additions
- Monitor subscriber restrictions

---

### BlocklistRemovedEvent

**Topic:** `blocklist_removed`

Emitted when a subscriber is removed from the blocklist.

**Fields:**
- `subscriber` (Address): Subscriber that was removed from blocklist
- `removed_by` (Address): Admin that authorized the removal

**Indexing Strategy:**
- Index by `subscriber` to track blocklist status

**Example Use Cases:**
- Track blocklist removals
- Monitor subscriber restrictions

---

## General Indexing Recommendations

### Event Consumption

1. **Subscribe to contract events** using Stellar RPC or Horizon API
2. **Filter by contract address** to get only subscription vault events
3. **Parse event topics** to identify event type
4. **Decode event data** using the schemas above

### Storage Strategy

- Store events in time-series database for historical analysis
- Maintain current state in relational database for fast queries
- Index by `subscription_id`, `subscriber`, `merchant`, and `token` addresses

### Error Handling

- Events are emitted after state changes succeed
- If a transaction fails, no event is emitted
- Monitor transaction status alongside events

### Privacy Considerations

- Events contain only addresses and amounts (no personal data)
- Metadata values are never emitted in events
- Addresses are pseudonymous but publicly visible on-chain
- Off-chain systems should implement additional privacy controls

---

## Example Event Flow

**Typical subscription lifecycle:**

1. `SubscriptionCreatedEvent` - Subscriber creates subscription
2. `FundsDepositedEvent` - Subscriber deposits initial funds
3. `SubscriptionChargedEvent` (recurring) - Billing engine charges subscription
4. `FundsDepositedEvent` (as needed) - Subscriber tops up balance
5. `SubscriptionPausedEvent` (optional) - Subscriber pauses temporarily
6. `SubscriptionResumedEvent` (optional) - Subscriber resumes
7. `SubscriptionCancelledEvent` - Subscriber or merchant cancels
8. `SubscriberWithdrawalEvent` - Subscriber withdraws remaining balance
9. `MerchantWithdrawalEvent` (periodic) - Merchant withdraws earnings

---

## Version History

- **v1.0** (2024-04-24): Complete event schema with stable field names and types for all lifecycle actions, fund movements, and administrative operations
