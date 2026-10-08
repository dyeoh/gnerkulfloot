# gnerkulfloot

A headless shop backend written in Rust. It's an HTTP/JSON API with no built-in
storefront. You bring the frontend (website, app, anything that can make HTTP
requests), and gnerkulfloot handles products, stock, orders, payments, shipping,
tax and email.

It's built to be small and boring to run:

- one binary, backed by one Postgres database;
- deploys as a container on a DigitalOcean droplet, or as a systemd service on
  any Linux box;
- scales out by running more copies behind a load balancer. Redis is optional
  and only needed once you run several copies.

Anything vendor-specific (payments, shipping, email, image storage) plugs in
through an **adapter** you choose in config, so you're never locked into one
company.

> **Status: early.** The foundation is in place: config, database, migrations,
> health checks, logging, money handling, Docker. Shop features are being built
> in the order below. This README only documents what actually works; upcoming
> sections are marked *planned*.

| Area | Status |
|------|--------|
| Server, config, database, migrations, health checks, Docker | ✅ working |
| Multi-currency money type | ✅ working |
| Admin first-time setup, logins, staff accounts, rate limiting | ✅ working |
| Products, variants, per-currency prices, stock, categories, search | ✅ working |
| Images (S3 / R2 / DO Spaces / local disk), resized to WebP | ✅ working |
| Checkout, orders, stock holds, tax rules, flat-rate shipping | ✅ working |
| Online payments via HitPay (DuitNow QR, Touch 'n Go, cards), staff mark-paid | ✅ working |
| Order emails and marketing campaigns | planned (next) |
| EasyParcel shipping, Redis (shared rate limits), OIDC login | planned |
| systemd unit, Caddy/nginx load balancer configs | planned |

---

## Quickstart

You need Docker.

```sh
docker compose up -d --build
curl localhost:8080/readyz      # → ready
```

This starts the API on port 8080 and a Postgres database whose data is kept in
a Docker volume. Database migrations run automatically on startup.

Then create your admin account; see [First-time setup](#first-time-setup).

To stop it: `docker compose down`. Add `-v` to delete the database too.

## Configuration

Settings come from two places, and the later one wins:

1. a TOML file, `gnerkulfloot.toml` by default (pick another with `--config` or
   the `GNK_CONFIG` env var). It's optional.
2. environment variables named `GNK__<SECTION>__<KEY>` (note the **double**
   underscores).

So `bind` in the `[server]` section can be set either way:

```toml
[server]
bind = "0.0.0.0:8080"
```
```sh
GNK__SERVER__BIND=0.0.0.0:8080
```

Copy [`gnerkulfloot.example.toml`](gnerkulfloot.example.toml) to get started.
Every key is listed there with a comment. The ones you're most likely to change:

| Key | Default | What it does |
|-----|---------|--------------|
| `database.url` | *(required)* | Postgres connection string. |
| `shop.default_currency` | `USD` | Currency used when a request doesn't name one. Any ISO 4217 code (`MYR`, `SGD`, `JPY`…). |
| `server.cors_origins` | `[]` | Origins of your storefront, e.g. `["https://shop.example.com"]`. Browsers can't call the API from other origins. |
| `server.log_format` | `text` | `json` in production, so log tools can parse it. |
| `server.migrate_on_start` | `true` | Turn off if you'd rather run `gnerkulfloot migrate` yourself during deploys. |
| `server.trusted_proxies` | `[]` | CIDRs of your load balancer/reverse proxy. **Set this when running behind one**, or every visitor looks like the proxy and shares one rate limit. |
| `rate_limit.global`, `rate_limit.auth` | 300/min, 10/min | Requests allowed per client IP. `auth` covers login, register and setup. |
| `auth.max_failed_logins`, `auth.lockout_minutes` | 5, 15 | Wrong passwords in a row before an account is locked, and for how long. |
| `auth.session_ttl_hours` | 720 | How long a login lasts (30 days). |
| `rate_limit.orders` | 10/min | Orders one IP can place. Each order holds stock, so this stops a script tying it all up. |
| `checkout.payment_window_minutes` | 1440 | How long an unpaid order holds its stock (24 h). With online payments confirming automatically, 30–60 is plenty; keep it long if staff confirm payments by hand. |
| `tax.prices_include_tax` | `false` | `true` if your prices already include tax (tax is then shown as "included", not added). |
| `tax.default_rate_bp`, `tax.default_name` | 0, `Tax` | Tax when no tax rule matches the address, in hundredths of a percent (6% = 600). |

Log verbosity follows `RUST_LOG`, e.g. `RUST_LOG=debug` or
`RUST_LOG=info,sqlx=warn`.

### Adapters
Each external service is picked with an `adapter` key in its section.

| Section | Adapters | Status |
|---------|----------|--------|
| `[storage]` | `local`, `s3` (AWS S3, Cloudflare R2, DigitalOcean Spaces, MinIO, SeaweedFS…) | ✅ |
| `[payments]` | `hitpay` (see [Payments](#payments)), or `none` (staff mark orders paid) | ✅ |
| `[shipping]` | `adapters = ["flat_rate"]`: rates you manage yourself (see [Checkout](#checkout-and-orders)) | ✅ |
| `[shipping]` | `easyparcel` | planned |
| `[mail.transactional]`, `[mail.marketing]` | `smtp`, `log` | planned |

### Image storage

**`local`** (the default) keeps images in a folder on the server and serves them
at `/media/…`. In Docker that folder is the `media` volume. It's the simplest
option, but every app instance needs to see the same folder, so switch to `s3`
before running more than one instance.

```toml
[storage]
adapter = "local"
path = "media"                                       # /data/media in the Docker image
public_base_url = "https://api.example.com/media"    # absolute if the storefront is on another domain
```

**`s3`** stores images in any S3-compatible bucket, and browsers fetch them
straight from the bucket or its CDN:

```toml
[storage]
adapter = "s3"
bucket = "shop-images"
access_key_id = "..."
secret_access_key = "..."            # better: set GNK__STORAGE__SECRET_ACCESS_KEY
public_base_url = "https://img.example.com"

# Cloudflare R2
region = "auto"
endpoint = "https://<account-id>.r2.cloudflarestorage.com"
# DigitalOcean Spaces:  region = "sgp1", endpoint = "https://sgp1.digitaloceanspaces.com"
# AWS S3:               region = "ap-southeast-1", no endpoint
```

The bucket must be **publicly readable** (or sit behind a CDN that is), because
image URLs point straight at it. On R2 that means a custom domain or the
r2.dev URL. On Spaces and S3 it means a public-read bucket policy or the CDN
endpoint. Our app is the only thing that writes to it.

Every uploaded image is checked by its actual content (JPEG, PNG, WebP or GIF;
anything else is refused), rotated upright, stripped of metadata such as phone
GPS tags, and saved as two WebP files: `large` (up to 1600 px) and `thumb` (up
to 400 px). File names come from the image content, so files never change and
are cached for a year, and uploading the same picture twice stores it once.

## First-time setup

A fresh install has no accounts. On startup, the server prints a one-time
**setup token** to its log:

```sh
docker compose logs app | grep setup_token
# … "setup_token":"c1JYOD…","message":"first-time setup pending: …"
```

Trade it for the admin account:

```sh
curl -X POST localhost:8080/v1/setup -H 'content-type: application/json' \
  -d '{"token":"<token>","email":"you@example.com","password":"a long password"}'
```

The response contains a session token, so you're logged in straight away. From
then on setup is switched off for good: the token stops working and
`GET /v1/setup` returns `{"required": false}`. A storefront or admin UI can
check that to decide whether to show a setup screen.

Prefer the command line? This creates an admin directly and also switches web
setup off:

```sh
docker compose exec app gnerkulfloot admin create --email you@example.com
```

For automated deployments you can fix the token in advance with
`GNK__SETUP__TOKEN` instead of reading it from the log.

## Command line

```
gnerkulfloot [serve]                    run the API and background sweeps (default)
    [--no-worker]                       …without the sweeps (run `worker` separately)
gnerkulfloot worker                     run only the background sweeps
gnerkulfloot migrate                    apply database migrations and exit
gnerkulfloot setup-token                print the first-time setup token, if setup is pending
gnerkulfloot admin create --email E     create an admin account (prompts for the password)
    [--role staff]                      …or a staff account
    [--password-stdin]                  read the password from stdin, for scripts
gnerkulfloot --help
```

All commands accept `--config FILE`.

## API basics

- **Health:** `GET /healthz` returns 200 while the process is alive. `GET /readyz`
  returns 200 only when the database is reachable too. Point load-balancer health
  checks at `/readyz`.
- **Errors** use the standard [problem details](https://www.rfc-editor.org/rfc/rfc7807)
  format:
  ```json
  { "type": "about:blank", "title": "Not Found", "status": 404 }
  ```
- **Logging in:** `POST /v1/auth/login` with `{"email", "password"}` returns
  `{"user", "token", "expires_at"}`. Send the token on later requests as
  `Authorization: Bearer <token>`. Tokens start with `gnk_`, so treat anything
  starting with that like a password.
- **Accounts:**

  | Endpoint | Who | What |
  |---|---|---|
  | `POST /v1/auth/register` | anyone | create a customer account (optional; guests can buy without one) |
  | `POST /v1/auth/login` | anyone | log in (customers, staff and admins alike) |
  | `POST /v1/auth/logout` | logged in | end this session |
  | `GET /v1/auth/me` | logged in | who am I |
  | `GET /v1/admin/users` | admin | list admin and staff accounts |
  | `POST /v1/admin/users` | admin | create a staff or admin account: `{"email", "password", "role": "staff"}` |

  Roles: **admin** manages everything, including accounts. **staff** handles
  day-to-day work (products, orders) but can't manage accounts. **customer**
  sees only their own orders.
- **Rate limits:** too many requests from one IP gets `429 Too Many Requests` with a
  `Retry-After` header (seconds). Five wrong passwords in a row lock that account
  for 15 minutes, whatever IP the attempts come from.
- **Request ids:** every response has an `x-request-id` header. The same id
  appears in the server logs, so quote it when reporting a problem.
- **Money** is always a whole number in the currency's smallest unit, plus the
  currency code. RM19.90 is `{"amount": 1990, "currency": "MYR"}`. ¥500 is
  `{"amount": 500, "currency": "JPY"}`. Never send decimals.

## Catalog

A **product** (e.g. "Baju Kurung Moden") has one or more **variants** (SKUs, e.g.
"Red / M"). A SKU has its own code, stock level, weight, and a price in each
currency you sell in. Products can be in any number of **categories**, which
can be nested.

### For shoppers

| Endpoint | What |
|---|---|
| `GET /v1/products` | active products. Query: `currency`, `q` (search), `category` (slug), `sort` (`relevance`, `newest`, `price_asc`, `price_desc`, `name`), `page`, `per_page` (max 100) |
| `GET /v1/products/{slug}` | one product with variants, images and categories. Query: `currency` |
| `GET /v1/categories` | all categories as a flat list, with `parent_id` for nesting |

- Without `currency`, prices are in `shop.default_currency`.
- Products are only listed in currencies they have a price in. There's no
  automatic conversion.
- Search matches names and descriptions, partial words, and small typos
  ("tudng" finds "Tudung").
- Shoppers see `in_stock: true/false`, never exact stock counts.
- Lists return `{"items", "page", "per_page", "has_more"}`.

### For staff (`/v1/admin/…`, staff or admin login)

| Endpoint | What |
|---|---|
| `GET /v1/admin/products` | all products including drafts. Query: `status`, `q` (name, slug or SKU code), `page`, `per_page` |
| `POST /v1/admin/products` | create: `{"name", "slug"?, "description"?, "status"?, "attributes"?, "category_ids"?}` |
| `GET` / `PATCH /v1/admin/products/{id}` | view everything about a product / change any of the fields above |
| `POST /v1/admin/products/{id}/skus` | add a variant: `{"code", "name"?, "options"?, "stock_available"?, "weight_g"?, "prices": [{"currency", "amount", "compare_at_amount"?}]}` |
| `GET` / `PATCH /v1/admin/skus/{id}` | view / change a variant. Sending `prices` replaces all its prices |
| `POST /v1/admin/skus/{id}/stock` | adjust stock: `{"delta": 5}` to add, `{"delta": -2}` to remove |
| `POST /v1/admin/products/{id}/images` | upload an image (body = the image file). Query: `alt`, `sku_id` |
| `PATCH` / `DELETE /v1/admin/images/{id}` | change `alt` / `position`, or remove |
| `POST /v1/admin/categories`, `PATCH` / `DELETE /v1/admin/categories/{id}` | manage categories. `"parent_id": null` moves one to the top level |

- New products start as `draft`. Set `"status": "active"` to publish, or
  `"archived"` to hide. Products are never deleted, so old orders can still
  point at them.
- Slugs are made from the name if you don't give one ("Baju Kurung (Red)" →
  `baju-kurung-red`).
- **Stock only moves by adjustments**, never by setting a number. The stock
  figure already has units held by unpaid orders taken off, so overwriting it
  after a recount would release those holds and oversell. To correct after a
  recount, adjust by the difference.

Uploading an image:

```sh
curl -X POST "localhost:8080/v1/admin/products/$PRODUCT_ID/images?alt=Front%20view" \
  -H "authorization: Bearer $TOKEN" -H "content-type: image/jpeg" --data-binary @photo.jpg
```

## Checkout and orders

### Setting up shipping and tax (staff)

Shipping uses **zones**: a zone is a set of regions (whole countries, or states
within one), and each zone has **rates**. A state-level zone beats a
country-wide one, so "East Malaysia" (Sabah, Sarawak) overrides a catch-all
"Malaysia".

```sh
# A zone and a rate: RM8 up to 5 kg, free from RM100
curl -X POST localhost:8080/v1/admin/shipping/zones -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"name": "Malaysia", "regions": [{"country": "MY"}]}'
curl -X POST localhost:8080/v1/admin/shipping/zones/$ZONE_ID/rates -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name": "Pos Laju", "currency": "MYR", "amount": 800, "max_weight_g": 5000, "free_over_amount": 10000}'

# 6% SST for Malaysia, with a 0% exception for postcodes starting 87
curl -X POST localhost:8080/v1/admin/tax-rules -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"name": "SST", "country": "MY", "rate_bp": 600}'
curl -X POST localhost:8080/v1/admin/tax-rules -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"name": "Duty free", "country": "MY", "postcode_prefix": "87", "rate_bp": 0}'
```

| Endpoint | What |
|---|---|
| `GET` / `POST /v1/admin/shipping/zones` | list zones (with their regions and rates) / create one: `{"name", "regions": [{"country", "state"?}]}` |
| `PATCH` / `DELETE /v1/admin/shipping/zones/{id}` | rename or replace regions / delete with its rates |
| `POST /v1/admin/shipping/zones/{id}/rates` | add a rate: `{"name", "currency", "amount", "min_weight_g"?, "max_weight_g"?, "free_over_amount"?}` |
| `PATCH` / `DELETE /v1/admin/shipping/rates/{id}` | change / remove a rate |
| `GET` / `POST /v1/admin/tax-rules`, `PATCH` / `DELETE /v1/admin/tax-rules/{id}` | tax rules: `{"name", "country", "state"?, "postcode_prefix"?, "rate_bp", "applies_to_shipping"?}` |

The most specific tax rule for the address wins (postcode prefix, then state,
then country). If none matches, `tax.default_rate_bp` applies.

### Buying (shoppers)

1. **Quote** the basket whenever it or the address changes. Nothing is
   reserved yet.

   ```sh
   curl -X POST localhost:8080/v1/checkout/quote -H 'content-type: application/json' -d '{
     "currency": "MYR",
     "lines": [{"sku_id": "…", "quantity": 2}],
     "destination": {"country": "MY", "state": "Selangor", "postcode": "50450"}
   }'
   ```

   The quote has the line prices, `shipping_options` (cheapest first), the
   chosen `shipping`, `tax` and `total`. To pick another shipping option, send
   its `id` back as `shipping_option_id`.

2. **Place the order.** Send an `Idempotency-Key` header: any unique string
   made when the shopper presses "Place order", reused if you retry. A retry
   then returns the same order instead of placing a second one.

   ```sh
   curl -X POST localhost:8080/v1/orders -H 'content-type: application/json' \
     -H 'idempotency-key: 6f1c…' -d '{
     "email": "siti@example.com", "currency": "MYR",
     "lines": [{"sku_id": "…", "quantity": 2}],
     "shipping_address": {"name": "Siti", "line1": "1 Jalan Ampang", "city": "Kuala Lumpur",
                          "state": "Selangor", "postcode": "50450", "country": "MY", "phone": "…"},
     "shipping_option_id": "flat_rate:…"
   }'
   ```

   The response is `{"order": {…}, "access_token": "ord_…"}`. The order starts
   as `pending_payment` and **holds its stock** until `expires_at`. Keep the
   `access_token`: it's the guest's key to their order and is never shown again.
   Shoppers who are logged in (`Authorization: Bearer …`) get the order linked to
   their account too.

3. **View the order:** `GET /v1/orders/{id}` with header `X-Order-Token: ord_…`
   (or `?token=ord_…`, handy in emailed links). Logged-in customers can skip the
   token for their own orders, and list them with `GET /v1/me/orders`.

The server prices everything itself. Clients send only SKU ids, quantities, the
address and the chosen option; anything price-like in the request is ignored.

**Errors your storefront should handle.** These come with a `code`:

| Status | `code` | Meaning | Extra fields |
|---|---|---|---|
| 409 | `out_of_stock` | not enough stock for a line | `sku_id`, `available` |
| 409 | `unavailable` | item not for sale, or no price in this currency | `sku_id` |
| 422 | `shipping_unavailable` | no shipping option for this address and currency | |
| 422 | `unknown_shipping_option` | the chosen option doesn't apply any more; re-quote | |
| 422 | `idempotency_key_reused` | the same key was sent with a different order | |

### Order statuses and expiry

| Status | Meaning |
|---|---|
| `pending_payment` | placed, stock held, waiting for payment |
| `paid` | payment confirmed by the payment provider, or by staff |
| `fulfilled` | shipped *(arrives with fulfilment)* |
| `cancelled` | cancelled by staff before payment; stock went back on sale |
| `expired` | not paid within `checkout.payment_window_minutes`; stock went back on sale |

Expiry is handled by background sweeps that every `serve` process runs every
30 seconds. They're safe to run on many instances at once.

Staff manage orders with `GET /v1/admin/orders` (query: `status`, `q` = order
number or part of an email, `needs_review=true`, `page`), `GET /v1/admin/orders/{id}`
(with its payments and any `review_reason`), and
`POST /v1/admin/orders/{id}/cancel` for unpaid orders.

## Payments

Online payments use **HitPay's hosted checkout**. The shopper picks DuitNow QR
(payable from Touch 'n Go and any Malaysian banking app), a card, or whatever
else you enable, and pays on HitPay's page. HitPay then tells us by webhook, and
the order turns `paid` by itself.

### Setting up HitPay

1. Create a HitPay account and switch on the payment methods you want.
2. **API key:** dashboard → Settings → API Keys.
3. **Webhook:** dashboard → Developers → Webhook Endpoints. Add
   `https://<your api domain>/v1/payments/hitpay/webhook`, subscribe to the
   `payment_request.completed` event, and copy that endpoint's **salt** (it's
   not the API key's salt).
4. Configure (start with the sandbox account and `sandbox = true`):

   ```toml
   [payments]
   adapter = "hitpay"
   api_key = "..."                    # better: GNK__PAYMENTS__API_KEY
   webhook_salt = "..."               # better: GNK__PAYMENTS__WEBHOOK_SALT
   sandbox = true                     # required: true for HitPay's sandbox, false for real money
   payment_methods = ["duitnow", "touch_n_go", "card"]   # optional; empty = all enabled in HitPay
   return_url = "https://shop.example.com/orders/{order_id}"   # where shoppers land after paying
   ```

   `sandbox` has no default on purpose: getting it wrong either takes test
   payments in production or real money while testing.

The webhook must be reachable from the internet. For local testing, use a
tunnel (e.g. `cloudflared tunnel --url http://localhost:8080`) and register its URL.

### Paying for an order (storefront)

After placing an order, start the payment and send the shopper to `checkout_url`:

```sh
curl -X POST localhost:8080/v1/orders/$ORDER_ID/payment -H "x-order-token: $ACCESS_TOKEN"
# → {"id": "…", "status": "pending", "checkout_url": "https://securecheckout.hit-pay.com/…", "amount": {…}}
```

Calling it again returns the same open checkout, so a double-pressed "Pay"
button is harmless. When the shopper comes back to `return_url`, show the
order (`GET /v1/orders/{id}`) and its status. Don't treat the return itself as
proof of payment: the order is only `paid` once HitPay confirms it.

### How payments are confirmed

- A webhook is only accepted with a valid HitPay signature (forged ones get
  `401`). Even then, we ask HitPay's API for the payment's real state and act on
  that, never on the webhook's contents.
- If a webhook goes missing, a background check asks HitPay again about payments
  still pending after two minutes.
- HitPay's checkout expires together with the order, so nobody can pay for an
  order whose stock was already released.
- If money arrives anyway for an expired or cancelled order, the order takes its
  stock back and becomes `paid`, as long as the stock is still there. If it's
  sold out, or the amount is wrong, or the order was already paid, the order gets
  a `review_reason` and shows up under `GET /v1/admin/orders?needs_review=true`
  for staff to refund or restock. Refunds are done in the HitPay dashboard for
  now.

### Payments outside the shop

Cash or a bank transfer? Staff mark the order paid:
`POST /v1/admin/orders/{id}/mark-paid`. This works with or without HitPay set
up. With `[payments] adapter = "none"`, it's the only way orders get paid.

**Payment errors your storefront should handle:**

| Status | `code` | Meaning |
|---|---|---|
| 422 | `payments_disabled` | the shop has no online payments set up |
| 409 | `order_not_payable` | the order isn't awaiting payment any more (paid, expired, cancelled) |
| 502 | `payment_provider_error` | HitPay couldn't be reached or refused the request; try again |

## Deployment

### Single droplet / any Docker host
Copy the repo (or just `compose.yml` + `Dockerfile`) to the server and run
`docker compose up -d --build`. Change the Postgres password in `compose.yml`
first, or point `GNK__DATABASE__URL` at a managed database (e.g. DigitalOcean
Managed Postgres) and remove the `postgres` service.

The container is ~65 MB, runs as a non-root user, and stops cleanly on
`docker stop`: in-flight requests finish first. With local image storage,
images live in the `media` volume; back it up along with the database.

### Bare metal with systemd, load balancing, scaling out *(planned)*
Coming with the deployment phase: a hardened systemd unit, Caddy and nginx
load-balancer configs, and a DigitalOcean Load Balancer recipe.

With `s3` image storage every instance is stateless, so you can already run
several copies against the same database. Migrations are safe to run from many
instances at once.

## Development

You need Rust (stable) and Docker.

```sh
# A throwaway Postgres for tests
docker run -d --rm --name gnk-pg -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres:17-alpine
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres

cargo install sqlx-cli --no-default-features --features postgres,rustls   # once
sqlx migrate run                             # compile-time query checks need the schema
cargo test                                   # each DB test gets its own fresh database
cargo fmt && cargo clippy --all-targets -- -D warnings
```

After adding or changing a SQL query, run `cargo sqlx prepare -- --all-targets`
and commit the `.sqlx/` folder. That's what lets Docker builds check queries
without a database.

To try the `s3` storage adapter without a cloud account, `docker compose --profile s3 up`
starts a local S3-compatible bucket (SeaweedFS). `compose.yml` has the matching
`GNK__STORAGE__*` settings commented out, ready to switch on.

To run the server locally against that database:

```sh
GNK__DATABASE__URL=$DATABASE_URL cargo run
```

Before changing code, read [AGENTS.md](AGENTS.md). It explains the design
decisions, naming conventions and patterns the codebase follows.

## Troubleshooting

- **Orders stay `pending_payment` after paying**: check the webhook is
  registered in HitPay for `payment_request.completed` and is reachable from the
  internet. Logs show `webhook signature is missing or wrong` (and HitPay sees
  `401`) when `payments.webhook_salt` isn't that endpoint's salt. Even without
  webhooks, the background check confirms payments within a few minutes.
- **`payment_provider_error` when starting a payment**: the logs show HitPay's
  answer. Usually a wrong `api_key`, or a sandbox key with `sandbox = false` (or
  the reverse).
- **Checkout says `shipping_unavailable`**: no zone covers the address, or the
  zone has no active rate in the order's currency for that parcel weight. Check
  `GET /v1/admin/shipping/zones`.
- **Orders expire before they're paid**: raise
  `checkout.payment_window_minutes`. Expired orders have already released their
  stock, so place a new order rather than reviving the old one.
- **Images upload but don't show on the storefront**: the image URLs are built
  from `storage.public_base_url`. With `local` storage and a storefront on
  another domain, make it absolute (`https://api.example.com/media`). With
  `s3`, check that the URL is publicly readable: open one in a private browser
  window.
- **Upload returns 415**: the file isn't a JPEG, PNG, WebP or GIF, whatever its
  name or Content-Type says.
- **Upload returns 413**: the file is larger than `server.body_limit_bytes`
  (10 MiB by default).
- **A product doesn't appear in the shop**: it must be `active`, have at least
  one active variant, and have a price in the currency being requested.
- **Everyone gets rate-limited at once behind a load balancer**: the app sees
  the proxy as the client. Add the proxy's address range to
  `server.trusted_proxies`.
- **Lost the setup token**: run `gnerkulfloot setup-token`, or restart the app
  and check the log again.
- **Locked out of the only admin account**: wait out the lockout, or create
  another admin from the server with `gnerkulfloot admin create`.
- **`missing field database` (or `url`) on startup**: no database URL was found. Set
  `GNK__DATABASE__URL` or `[database] url` in your config file.
- **`/readyz` returns 503**: the app is running but can't reach Postgres. Check
  the URL, the network/firewall, and that the database is up.
- **The browser says "blocked by CORS policy"**: add your storefront's exact
  origin (scheme + host + port) to `server.cors_origins`.
