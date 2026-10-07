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
| Products, images (S3 / R2 / DO Spaces / local disk) | planned (next) |
| Checkout, orders, stock reservation, tax, flat-rate shipping | planned |
| DuitNow QR payments + payment confirmation | planned |
| Order emails and marketing campaigns | planned |
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

Log verbosity follows `RUST_LOG`, e.g. `RUST_LOG=debug` or
`RUST_LOG=info,sqlx=warn`.

### Adapters *(planned)*
Each external service is picked with an `adapter` key in its section. This table
fills in as adapters land.

| Section | Adapters |
|---------|----------|
| `[storage]` | `s3` (AWS, Cloudflare R2, DigitalOcean Spaces, MinIO), `local` |
| `[payments]` | `duitnow_qr` |
| `[payments.confirmation]` | `manual` (official TnG/gateway APIs later) |
| `[shipping]` | `flat_rate`, `easyparcel` |
| `[mail.transactional]`, `[mail.marketing]` | `smtp`, `log` |

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
gnerkulfloot [serve]                    run the API (default)
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

## Deployment

### Single droplet / any Docker host
Copy the repo (or just `compose.yml` + `Dockerfile`) to the server and run
`docker compose up -d --build`. Change the Postgres password in `compose.yml`
first, or point `GNK__DATABASE__URL` at a managed database (e.g. DigitalOcean
Managed Postgres) and remove the `postgres` service.

The container is ~56 MB, runs as a non-root user, and stops cleanly on
`docker stop`: in-flight requests finish first.

### Bare metal with systemd, load balancing, scaling out *(planned)*
Coming with the deployment phase: a hardened systemd unit, Caddy and nginx
load-balancer configs, and a DigitalOcean Load Balancer recipe.

Every instance is stateless, so you can already run several copies against the
same database. Migrations are safe to run from many instances at once.

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

To run the server locally against that database:

```sh
GNK__DATABASE__URL=$DATABASE_URL cargo run
```

Before changing code, read [AGENTS.md](AGENTS.md). It explains the design
decisions, naming conventions and patterns the codebase follows.

## Troubleshooting

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
