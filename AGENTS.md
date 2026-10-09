# Working on gnerkulfloot

This guide is for anyone changing the code, human or AI agent. It explains *why* the
system is shaped the way it is, how we name things, which patterns we stick to,
and how we write docs. If you're only trying to *run* the shop, read
[README.md](README.md) instead.

Short version: **keep it simple, keep Postgres in charge, put every external
service behind a trait, and never use floats for money.**

---

## 1. Design considerations

### Keep it simple (one crate, one binary)
gnerkulfloot is one Cargo crate that builds one binary. The binary is the API server,
the background worker, the migration runner and the admin CLI. No microservices,
no workspace, no message broker. When something feels like it needs a new
service, it probably needs a new module.

### Postgres is the source of truth
Every fact the shop depends on lives in Postgres: stock, orders, payments,
sessions, jobs. Each app instance is stateless and disposable, so you can run one
or ten behind a load balancer and they all agree.

**Redis is optional and never what guarantees correctness.** When configured, it
does three things:

1. shares rate-limit counters between instances,
2. hands out distributed locks so only one instance runs singleton jobs (the
   payment reconciliation, the campaign sender),
3. optionally sheds excess traffic for a "hot" SKU during a flash sale before it
   reaches the database.

If Redis disappears, the shop still sells correctly. It just rate-limits per
instance instead of globally.

### Inventory: how we never sell the last item twice
Stock is reserved inside the order-creation transaction with one conditional
update:

```sql
UPDATE skus SET stock_available = stock_available - $qty
WHERE id = $sku AND stock_available >= $qty
RETURNING stock_available;
```

Postgres row locking makes this atomic. If two requests race for the last unit,
one updates a row and the other updates zero rows and gets "out of stock". This
holds no matter how many app instances are running. With multiple lines in an
order, we update SKUs in ascending id order so two orders can't deadlock each
other.

An order waiting for payment holds its stock until `expires_at`. A background job
releases expired reservations. A payment that confirms *after* expiry is flagged
for an admin to review, never silently accepted.

### Adapters behind traits
Anything that talks to an outside vendor sits behind an **adapter trait**. The
trait is our vendor-neutral contract. Each implementation is an adapter for one
vendor or method.

We say *adapter*, not *provider*, on purpose: the "provider" is the vendor itself
(Touch 'n Go, EasyParcel, Cloudflare). The adapter is our code that speaks to it.
Keeping the words separate keeps the code vendor-agnostic in spirit as well as
in structure.

| Concern              | Trait                 | Adapters                                   |
|----------------------|-----------------------|--------------------------------------------|
| Payments             | `PaymentAdapter`      | `Hitpay` (more gateways, or TnG direct, later) |
| Shipping             | `ShippingAdapter`     | `FlatRate`, `EasyParcel`                   |
| Mail                 | `MailAdapter`         | `Smtp`, `Log` (dev/tests)                  |

Two deliberate exceptions:

- **Image storage** uses the `object_store` crate directly. It's already a
  vendor-neutral abstraction over S3, R2, DO Spaces, MinIO and local disk, so
  wrapping it again would add nothing.
- **Infrastructure mechanisms** (`RateLimiter`, `DistLock`) aren't vendor
  integrations, so they keep plain role names. Their implementations are
  `MemoryRateLimiter`/`RedisRateLimiter` and `RedisLock`/`PgAdvisoryLock`.

Config picks the adapter with an `adapter` key, e.g. `[storage] adapter = "s3"`.
Where several adapters can be active at once (shipping: checkout merges their
options), the key is a list: `[shipping] adapters = ["flat_rate", "easyparcel"]`.
**To add an adapter:**

1. write a struct implementing the trait, in its own file next to the trait;
2. add a variant to that section's config enum in `config.rs`;
3. construct it in the factory function next to that enum;
4. document its config keys in the README adapter table.

Nothing else in the codebase should need to know it exists.

### Money is currency-agnostic
All money is `Money { amount: i64, currency: Currency }`, where `amount` is in the
currency's *minor units* (sen, cents, or whole yen, since JPY has none). The
currency carries its own exponent, so rounding is always correct for that
currency.

- There are no floats anywhere money is involved.
- Adding two amounts in different currencies returns an error. It never
  silently converts.
- Every money column is stored as a pair: `<name>_amount BIGINT` and
  `<name>_currency CHAR(3)`. The exception is a table whose amounts must all
  share one currency, like `orders`: it has a single `currency` column, so a
  mismatch can't even be written down.
- A SKU has one price per currency (`sku_prices`). There's no automatic FX.
- An order is in exactly one currency, and every line, fee, tax and payment
  on it uses that currency.
- Adapters say which currencies they support, and checkout only offers
  compatible ones.

### Payments and how confirmation works
Online payments go through a payment gateway behind `PaymentAdapter`. The first
is **HitPay**: its hosted checkout offers DuitNow QR (payable from Touch 'n Go
and any Malaysian banking app), cards and more, and it tells us about payments
by signed webhook. We chose a gateway because a plain merchant QR has no API
that says "this money arrived"; only the acquirer or a licensed gateway can.

An order is marked paid by exactly two things:

1. **The provider's API.** A webhook is only a signed hint that something
   changed. We check its signature, then ask the provider's API for the
   payment's real state and act on that. We never act on the webhook body:
   HitPay's payload shapes vary, and a body can't be trusted just because its
   signature is right. A reconciliation sweep asks the API again for payments
   still pending after two minutes, in case a webhook was lost.
2. **A staff member**, for money taken outside the shop (cash, bank transfer),
   via `POST /v1/admin/orders/{id}/mark-paid`.

The shopper being redirected back from the checkout page proves nothing and
never changes an order.

When money arrives for an order that can't simply be marked paid, we don't
guess: a lapsed (expired or cancelled) order tries to hold its stock again and
is paid if it can, and anything else (stock sold out meanwhile, wrong amount,
paid twice) sets `orders.review_reason` for a person to resolve. Applying a
provider's verdict is idempotent: a payment leaves `pending` only once.

**We never scrape for payment status.** Parsing notification emails, forwarding
phone notifications, or screen-scraping apps are all off the table. They break
silently when the wording changes, and they're easy to spoof. A payment is
confirmed by a person or by an authenticated API, nothing else.

### Tax
Tax is resolved in order:

1. the payment provider, if it calculates tax;
2. the most specific matching rule in `tax_rules` (country → state → postcode
   prefix);
3. the configured flat default rate.

Tax is calculated per line in minor units and snapshotted onto the order, so
editing a rule later never changes past orders.

### Security posture
- Passwords are hashed with **argon2id**.
- Session tokens are random and opaque. Only their SHA-256 is stored, so a
  database leak doesn't leak live sessions.
- The client IP is taken from `X-Forwarded-For` **only** when the request comes
  from a configured trusted proxy. Otherwise anyone could spoof their IP to dodge
  rate limits.
- Rate limits are tiered: tight on login, setup and order creation, looser
  elsewhere.
- Webhooks are signature-verified and de-duplicated by event id. Unsubscribe
  links are signed.
- First-time setup uses a one-time token and switches itself off permanently once
  an admin exists.

---

## 2. Naming conventions

**Rust code**
- Standard Rust casing: `snake_case` functions and modules, `PascalCase` types,
  `SCREAMING_SNAKE_CASE` constants.
- Modules are named after domain nouns: `catalog`, `checkout`, `payments`,
  `shipping`, `mail`.
- Vendor-facing traits are `<Concern>Adapter`: `PaymentAdapter`,
  `ShippingAdapter`, `MailAdapter`. Use "provider" only for the vendor itself,
  never for our types.
- Adapter structs are named after the vendor or method, with no suffix, and live
  in the concern's module: `payments::Hitpay`, `shipping::EasyParcel`,
  `mail::Smtp`.
- HTTP handlers are `verb_noun`: `create_order`, `list_products`, `mark_order_paid`.
- Error enum variants are `PascalCase` nouns describing what went wrong:
  `OutOfStock`, `CurrencyMismatch`, `NotFound`.

**Database**
- Tables are plural `snake_case`: `orders`, `order_lines`, `sku_prices`.
- Primary keys are `id UUID`. Foreign keys are `<singular>_id`, e.g. `order_id`.
- Money is a column pair: `total_amount BIGINT`, `total_currency CHAR(3)`, or
  `<name>_amount` columns sharing one `currency` column when they must match.
- Timestamps are `<event>_at TIMESTAMPTZ`: `created_at`, `paid_at`, `expires_at`.
- Status columns use Postgres text with a `CHECK` constraint, mirrored by a Rust
  enum.
- Migrations live in `migrations/` as `YYYYMMDDHHMM_short_description.sql`. Never
  edit a migration that has been released; add a new one.

**Config**
- Keys are `snake_case`, grouped by section: `[storage]`, `[mail.marketing]`.
- Every section that selects an adapter has an `adapter` key.
- Any key can be overridden by an env var: `GNK__SECTION__KEY` (double
  underscores).

**HTTP**
- Paths are plural nouns under a version: `/v1/products/{slug}`.
- Admin routes live under `/v1/admin/...`.
- JSON fields are `snake_case`.

---

## 3. Patterns

**Request flow: handler → service → query.**
Handlers parse input, check auth, call a service function and shape the response.
Business rules live in service functions, which take a `&mut PgConnection` or a
transaction, so tests can call them without HTTP.

- A service that calls an adapter over the network (a payment provider, a
  shipping quote) takes `&PgPool` instead and opens its own transactions
  around the call, never across it. A transaction held open during an HTTP
  call holds its row locks for as long as the provider takes to answer.
- Services take the config sections and adapters they use (`&TaxConfig`,
  `Option<&dyn PaymentAdapter>`, `checkout::Pricing`), never `AppState`.
  `AppState` (`state.rs`) belongs to handlers, the worker and `main`.

**SQL.**
- Use `sqlx::query!` / `query_as!` by default. They're checked against the real
  schema at compile time. After changing a query, run `cargo sqlx prepare` and
  commit `.sqlx/`, so builds (including Docker) don't need a database.
- Use `sqlx::QueryBuilder` only for genuinely dynamic queries (filters, sorting,
  admin search). Always `push_bind` values. **Never** format user input into SQL.

**Transactions.** One use-case = one transaction. Create order, reserve stock and
record idempotency key all commit together or not at all.

**Background work goes through the `jobs` table**, not `tokio::spawn`. A spawned
task dies with its instance; a job row survives restarts and is picked up by
whichever instance is free (`FOR UPDATE SKIP LOCKED`).

- **Queue jobs inside the transaction that causes them**
  (`jobs::enqueue(&mut tx, &Job::…)`). The job then exists exactly when the
  change does: no email for an order that rolled back, no lost email when the
  process dies right after commit.
- **Add a job** by adding a `Job` variant (its fields are stored as JSON, so
  keep them backward compatible with jobs already queued), a `dedupe_key` if it
  must only happen once, and a match arm in `jobs::run`.
- **Jobs run at least once.** A worker that dies after doing the work but before
  recording it means the job runs again. Make jobs harmless to repeat (an
  occasional duplicate email is fine). Anything that moves money must not be a
  job.
- **Order is per batch, not global.** One worker runs a claimed batch in queue
  order, but two workers can each claim a job at the same instant. Don't rely on
  strict ordering between jobs.

Two things deliberately don't use the queue:

- **Periodic sweeps** in `worker.rs` (expiring unpaid orders, purging old
  sessions and idempotency keys). They're idempotent, each step commits
  atomically, and they claim rows with `SKIP LOCKED`, so running them on every
  instance is safe and losing a pass to a restart costs nothing.
- **Housekeeping of in-memory state** that dies with the process anyway, like
  the in-memory rate limiter's sweep of idle clients.

**Auth in handlers.** Take an extractor argument: `CurrentUser`, `StaffUser` or
`AdminUser` (in `auth::extract`). Don't check roles by hand inside handlers.
Endpoints that accept passwords or tokens go in a `credential_routes()` router,
so they get the strict `auth` rate-limit tier.

**Errors.** Each module has a `thiserror` enum. `AppError` in `error.rs` converts
everything into RFC 7807 `application/problem+json` responses, in one place. Don't
build error responses by hand in handlers. Never leak internal error text to
clients. Log it, and return a generic message with the request id.

- When a client needs to act on *which* thing failed, use `AppError::Rejected`
  with a stable `code` and the details as extra members, e.g.
  `{"code": "out_of_stock", "sku_id": "…", "available": 0}`. Storefronts branch
  on `code`, never on the `detail` text.
- Simple admin-managed reference data (shipping zones and rates, tax rules)
  shares `error::DataError` instead of each growing an identical enum.

**Prices come from the server.** Clients send ids, quantities and choices
(SKU, shipping option, address), never amounts. Placing an order re-runs the
quote server-side, and any price-like fields in a request are ignored.

**Stock.** Change `stock_available` only with relative, conditional updates
(`SET stock_available = stock_available + $delta WHERE … AND stock_available + $delta >= 0`).
Never write an absolute number: the column already has reserved units taken off,
so overwriting it releases reservations and oversells.

**Partial updates (PATCH).** Use `COALESCE($n, column)` for "leave alone if
missing". For fields where `null` means "clear it", deserialize with
`catalog::double_option` and update with `CASE WHEN $set THEN $value ELSE column END`.

**Money.** Construct and combine amounts only through `Money`. If you're writing
`amount * rate / 100` by hand, use the helper on `Money` instead.

**Idempotency.** Endpoints that create something costly (orders, payments)
accept an `Idempotency-Key` header and replay the original response for repeats.

**Tests.**
- Every adapter trait has a test double (the `Log` mail adapter, the in-memory limiter, etc.).
- Every new endpoint gets a `#[sqlx::test]` integration test, which gives each
  test its own fresh database.
- Concurrency-sensitive code (stock, payments) gets a test that hammers it in
  parallel.

**API docs.** Annotate handlers and DTOs with `utoipa` so `/openapi.json` stays
accurate for frontend developers.

---

## 4. Doc style

- **`//!` at the top of every module**: two or three sentences on what the module
  is for and how it fits in.
- **`///` on every public item**: what it does and *why it exists*, not a narration
  of the code. Add `# Errors` when the failure cases aren't obvious from the
  types, and an `# Examples` block for small public helpers like `Money`.
- **Inline comments explain intent and invariants**, the things the code can't say
  by itself. Good: `// Lock SKUs in id order so concurrent orders can't deadlock.`
  Bad: `// loop over lines`.
- Write plainly. Short sentences, no marketing, no "simply" or "just".
- **Docs change with the code.** If a change affects behaviour, config or
  deployment, update README.md in the same commit. If it changes a design decision
  or convention, update this file.

---

## 5. Commits

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/),
the format [commitizen](https://commitizen.github.io/cz-cli/) produces. The repo's
`.czrc` points commitizen at `cz-conventional-changelog`, so running `git cz`
walks you through it. Writing the message by hand is fine too:

```
<type>(<scope>): <summary>

<body: what changed and why, wrapped at 72 columns>

<footer: BREAKING CHANGE: …, Closes #12>
```

- **type**: what kind of change it is.

  | type | use for |
  |---|---|
  | `feat` | a new capability for users of the API |
  | `fix` | a bug fix |
  | `docs` | documentation only |
  | `refactor` | restructuring with no behaviour change |
  | `perf` | faster or leaner, same behaviour |
  | `test` | adding or fixing tests only |
  | `build` | dependencies, Cargo, Dockerfile |
  | `ci` | CI pipelines |
  | `chore` | anything else that doesn't touch the shipped code |

- **scope**: optional. Use the module or area: `auth`, `ratelimit`, `money`,
  `payments`, `catalog`, `checkout`, `deploy`, `db`.
- **summary**: imperative, lowercase, no full stop, under ~70 characters.
  Write `add session lockout`, not `Added session lockout.`
- **body**: explains *why*, and anything a reviewer would otherwise have to ask.
- **Breaking changes** to the API, config keys or database get a `!` after the
  type/scope (`feat(auth)!: …`) and a `BREAKING CHANGE:` footer saying what to do.
- **One logical change per commit**, and every commit should build and pass
  tests.

### Versioning and releases
The version in `Cargo.toml` follows [semver](https://semver.org) and is bumped
by [release-plz](https://release-plz.dev) from commit types. **Don't edit the
version or `CHANGELOG.md` by hand.**

1. Every push to `main` updates an open `chore: release vX.Y.Z` PR with the
   next version, `Cargo.lock` and changelog entries since the last `vX.Y.Z` tag.
2. Merging that PR tags `vX.Y.Z`, creates a GitHub Release and pushes
   `ghcr.io/dyeoh/gnerkulfloot:X.Y.Z` (plus `X.Y` and `latest`).

How a commit moves the version:

| commit | while on 0.x | from 1.0 |
|---|---|---|
| `fix`, `perf` | patch | patch |
| `feat` | patch | minor |
| `!` / `BREAKING CHANGE:` | minor | major |
| anything else | no release | no release |

This is why commit types matter: a `feat` written as `chore` never ships in a
release, and a breaking change without `!` gets a version that lies about it.
CI lints every commit on a pull request against Conventional Commits
(`.commitlintrc.json`). Config lives in `release-plz.toml`, workflows in
`.github/workflows/`.

## 6. Before you commit

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo sqlx prepare --check   # only if queries changed
cargo test
```

CI (`.github/workflows/ci.yml`) runs the same checks on every pull request and
on `main`, against a fresh Postgres.
