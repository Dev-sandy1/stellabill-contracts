# View helper: billing statements by subscription

This contract exposes a dedicated read-only view for billing history pages:

- `get_sub_statements_offset(subscription_id, offset, limit, newest_first)`
- `get_sub_statements_cursor(subscription_id, cursor, limit, newest_first)`

## Data model

Each successful charge appends an immutable `BillingStatement` row keyed by:

- `(subscription_id, sequence)`
- `sequence` is monotonic per subscription and starts at `0`

Rows include:

- `subscription_id`
- `sequence`
- `charged_at`
- `period_start`
- `period_end`
- `amount`
- `merchant`
- `kind` (`Interval`, `Usage`, `OneOff`)

## Ordering

Both APIs support deterministic ordering via `newest_first`:

- `true`: reverse chronological by sequence (latest first)
- `false`: chronological by sequence (oldest first)

Because sequence values are append-only and never rewritten, pagination remains stable across new inserts.

## Pagination strategies

### Offset/limit

Use for classic page-number style UIs.

- `offset` is zero-based
- `limit` must be greater than `0`
- response includes `total` and optional `next_cursor`

### Cursor

Use for infinite-scroll or "load more" pages.

- `cursor` is an inclusive sequence index
- for first page pass `None`
- response includes `next_cursor`; `None` means end of history

## Client usage examples

Offset/limit:

```rust
let page = client.get_sub_statements_offset(
    &subscription_id,
    &0u32,
    &20u32,
    &true,
);
```

Cursor:

```rust
let first = client.get_sub_statements_cursor(
    &subscription_id,
    &None::<u32>,
    &20u32,
    &true,
);

let next = client.get_sub_statements_cursor(
    &subscription_id,
    &first.next_cursor,
    &20u32,
    &true,
);
```
