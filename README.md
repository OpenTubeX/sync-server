# OpenTubeX Sync Server
Server to synchronize OpenTubeX data between devices, including subscriptions, playlists, watch history, profiles, sessions, and settings.

OpenTubeX clients can use the encrypted sync API exposed by this server. In
that mode all synchronized content is encrypted on the client with a separate
privacy passphrase and the server stores one opaque, revisioned document per
account. Documents are padded in 64 KiB blocks to reduce size leakage. The
operator can still observe account activity, IP addresses, request timing, and
approximate encrypted document size, but cannot read its contents.
LibreTube-compatible plaintext endpoints remain available for older clients.
When an account uploads its first encrypted document, its account-linked
legacy plaintext sync records are removed. This cannot erase server backups
that may already exist. After that first upload, plaintext sync endpoints are
rejected for the account so an older client cannot accidentally repopulate
readable data.

## Deprecated playback-speed API

The dedicated `/v1/channel_playback_speeds` endpoints and encrypted
`playbackSpeeds` collection are deprecated. Current OpenTubeX clients store all
saved channel preferences, including playback speeds, in the encrypted
`settings` collection.

Both deprecated forms remain fully functional during the client migration
period. Their database table, encrypted collection support, legacy migration,
and cleanup logic must only be removed after supported clients no longer use
them. Responses from the dedicated plaintext endpoints include the standard
`Deprecation` header; no removal date has been scheduled.

This project is based on the [LibreTube sync server](https://github.com/libre-tube/sync-server).

## Running
It's recommended to run the app with Docker.

There are multiple prebuilt Docker images, built for ARM64 and x86:
- `ghcr.io/opentubex/sync-server:latest-postgres`: uses PostgreSQL as database backend
- `ghcr.io/opentubex/sync-server:latest-sqlite`: uses SQLite as database backend

For reference, please see the example `docker-compose` files at [docker-compose.yml](./docker-compose.yml) and [docker-compose.postgres.yml](./docker-compose.postgres.yml).

After you chose the correct `docker-compose.yml` for your use case, just run `docker compose up`.

### Configuration

There are two ways to configure `sync-server`

- TOML file

  If you want to use TOML, just place a `config.toml` in the working directory of the server.

- Environment variables

  The configuration can also be done through environment variables. Casing doesn't matter here.

### Configuration Reference

| Config option                   | Description                                          | Default | Example              |
| ----------------------          | ---------------------------------------------------- | ------- | -------------------- |
| `database_url`                  | Connection string for the database                   | None    | sqlite://./db.sql    |
| `secret_key`                    | Used to sign authentication tokens. Required, min. 32 bytes | None | output of `openssl rand -hex 32` |
| `username_secret`               | Used to derive account name hashes. Set it so that `secret_key` stays rotatable | falls back to `secret_key` | output of `openssl rand -hex 32` |
| `trust_forwarded_for`           | Derive rate limiting client addresses from `X-Forwarded-For`. Required behind a reverse proxy | `false` | `true` |
| `trusted_proxy_hops`            | Number of proxies in front of this server, used to pick the right `X-Forwarded-For` entry | `1` | `2` |
| `allow_registration`            | Whether to allow registering on this server          | `true`  | `false`              |
| `validate_submitted_metadata`   | Whether to check incoming video data against YouTube | `true`  | `false`              |
| `migration_approval`            | Exact comma-separated pending migration versions approved for an existing database after separately verifying a backup | None | `202607211800000000` |

### Running behind a reverse proxy

`/account/register` and `/account/login` are rate limited per client address. That
address is the immediate peer by default, which is only correct when the server is
directly reachable.

The example compose files publish to `127.0.0.1`, so a reverse proxy is the normal
setup — and there every request arrives from the proxy's own address. Without
extra configuration all clients would then share a single bucket and legitimate
users would rate limit each other. Set `trust_forwarded_for = true` so the limit
is applied per real client:

```yaml
environment:
  - "TRUST_FORWARDED_FOR=true"
```

The address is taken from the *last* `X-Forwarded-For` entry, which is the one
your proxy appended, so a client cannot pick its own bucket by sending the header
itself. Only enable this when the server is not reachable directly, otherwise a
client can do exactly that.

If you have more than one proxy in the chain, set `trusted_proxy_hops` to how many
there are. Each proxy appends an entry, so with two the last one is the inner
proxy's address rather than the client's.

**Cloudflare Tunnel counts as two hops.** The Cloudflare edge adds the client
address and `cloudflared` appends its own, so `trusted_proxy_hops = 2` is correct
for a `cloudflared` deployment. Leaving it at `1` is worse than having no
per-client limit: the trailing entry varies between edge addresses, so requests
scatter across buckets and the limit becomes both bypassable and randomly
triggered for innocent clients.

Verify your value rather than assuming it. Send more than
`MAX_REQUESTS_PER_WINDOW` failed logins from one client and check that the
responses switch to `429` and stay there. If they alternate between `429` and
normal responses, the resolved address is not stable and the hop count is wrong.

This limiter is per process and best-effort. Rate limiting at the proxy as well
is still recommended, and is required if you run multiple replicas.

### Running as non-root

The image runs as uid `10001`. A bind-mounted host directory keeps its host
ownership and shadows the ownership set in the image, so for SQLite deployments
prepare the data directory once before the first start:

```sh
mkdir -p ./data
sudo chown -R 10001:10001 ./data
```

Without this the server cannot create `db.sqlite` or its WAL sidecar files. Note
that the failure surfaces as a database connection timeout rather than an obvious
permission error. If you are upgrading from an older image that ran as root, run
the same command against your existing `./data` directory — this changes
ownership only and does not touch the database contents.

If you cannot use `sudo`, or your data directory is already owned by your own
user, override the container user instead of changing ownership:

```yaml
services:
  sync:
    user: "1000:1000" # your own uid:gid, from `id -u`:`id -g`
```

The server only needs to write inside `/app/data`, so any uid that owns the data
directory works. This still avoids running as root.

### Secrets

The server refuses to start if `secret_key` is missing, shorter than 32 bytes, or
left at a placeholder such as `changeme`. Generate one with `openssl rand -hex 32`.

Accounts are looked up by `HMAC(username)`, so changing the secret that derives
that hash makes every existing account unreachable. Set `username_secret`
explicitly (initially to the same value as `secret_key`) so that `secret_key`
itself can later be rotated — for example after a suspected leak — without
locking anyone out.

Existing databases never apply pending migrations implicitly. After you create and verify
a separate backup, `migration_approval` must exactly match every pending version.
Versions are digits only, without the hyphens used in the migration directory
names — the startup error prints the exact value to use, so the simplest approach
is to start the server once and copy it from the log. SQLite
also creates a consistent `*.pre-migration-<version>` backup immediately before changing
the schema. Remove the approval after deployment so it cannot authorize a later migration.

`oidc` section of the configuration (all options are required to use OIDC):
| Config option                   | Description                                                    | Default    | Example                  |
| ----------------------          | ----------------------------------------------------------     | ---------- | ------------------------ |
| `provider_url`                  | Base URL of the OIDC provider                                  | None       | https://auth.example.com |
| `client_id`                     | Client ID of the OAuth app configured at the OIDC provider     | None       | SecretOauthAppClientID   |
| `client_secret`                 | Client secret of the OAuth app configured at the OIDC provider | None       | SomeVerySecureString64   |
| `app_url`                       | Public URL to the `sync-server` instance                       | None       | https://sync.example.com |

The OIDC app must be configured to allow redirects to `<your_app_url>/v1/account/oidc/authenticate/callback` and
`<your_app_url>/v1/account/oidc/authenticate/delete/callback`.

## API Documentation
- Start the app, e.g. with `cargo run`.
- The documentation can now be found at `http://localhost:8080/docs`.

### Authentication
There are two ways to login:
- via username and password, i.e. credentials are stored on the server
- via OpenID Connect, i.e. authentication is delegated to an OIDC server. Only works if you configure the OIDC provider as described in [the configuration reference](configuration-reference)

After registering or logging in, you receive a `jwt` as response.

This `jwt` must be passed either as `Authorization` cookie or header for authenticated requests, e.g. for creating subscriptions.
For example:
- Header: `Authorization: abcdefghijklmnopqrtuvwxyz`
- Cookie: `Authorization=abcdefghijklmnopqrtuvwxyz`

### Account sessions

Capability `account_sessions: 1` means authentication tokens are backed by
stored account sessions. Registration, password login, and OIDC login create an
active session. Secure pairing creates a provisional session that becomes active
only when the receiving device consumes the approved pairing payload. A token
contains that session's ID in its `jti`
claim, and authenticated requests fail after the session is revoked or expires.
Tokens issued before this capability was added do not contain `jti`. For
accounts present during the migration, the server accepts a still-valid legacy
token and creates its session on first use. New accounts require session-bound
tokens. The server
derives stable session and device IDs from a SHA-256 digest of the token and
does not store the token itself. Revoking that session leaves a tombstone until
the token expires, so the same token cannot recreate the session.

Current clients send a random 16-byte base64url device ID when they authenticate.
The field is optional so older clients can continue to register and sign in; the
server assigns their device ID. Clients
encrypt the user-visible device name, operating system, system release, and
architecture with the enhanced-privacy key, then update the session with the
ciphertext. Clients can replace that ciphertext to rename any active device.
The server retains creation, last-active, and expiry times. It writes
last-active changes at most once every five minutes.

The endpoints are:

- authenticated `GET /v1/account/sessions` to list active sessions and whether the account supports password login
- authenticated `PATCH /v1/account/sessions/{id}` to store encrypted device information; `current` may be used as the ID for the requesting session
- authenticated `DELETE /v1/account/sessions/{id}` to revoke one session
- authenticated `PUT /v1/account/password` to change a password, revoke every existing session, and return a replacement JWT for the requesting device

Expired and revoked sessions never authenticate. A background task removes them
after expiry, normally within one hour. Password changes verify the current
password and update its Argon2 hash in the same transaction that rotates the
requesting session, advances the account's session generation, and revokes the
rest. The generation check also rejects a concurrent login that verified the old
password but had not created its session yet. Password changes reject every legacy token
that has not yet created a session. This lets operators deploy the feature
without signing out old clients, while password changes still invalidate all
other access.

### Enhanced privacy sync

`GET /health` returns the server's capabilities alongside its health status.
Authenticated clients read the collection manifest from `GET /v1/encrypted_sync`
and transfer individual opaque collections through `GET` and `PUT`
`/v1/encrypted_sync/{collection}`. Each collection has an independent revision;
an update with a stale revision fails with HTTP 409 without blocking unrelated
collections. The server never receives the privacy passphrase or plaintext sync
content. Before the first encrypted collection is uploaded, the manifest's
`legacy_data` flag tells the client to pull and merge the old plaintext records.
Each legacy domain is removed transactionally only after its matching encrypted
collection is stored, so an interrupted migration can safely resume.
Ciphertext uploads have collection-specific limits: 2 MiB for settings, 8 MiB
for profiles, playback speeds, and versioned or legacy sessions, 16 MiB for
subscriptions and playlist bookmarks, and 64 MiB for playlists and history.
The combined active encrypted collections for one account cannot exceed 128 MiB.

### Secure device pairing

Capability `key_pairing: 1` advertises passwordless device pairing for
enhanced-privacy sync. A receiving device anonymously creates a pending
session, then shows a QR or text code. An already authenticated device claims
the session for its account and approves it. During the claim, the server creates
a provisional account session and mints a JWT for the receiving device. The approving
device encrypts that JWT,
the account name, privacy key, privacy salt, and a six-digit verification code
before uploading one opaque relay payload.

The server stores the session ID, SHA-256 recipient-token hash, recipient public
key, device IDs, receiving-device display name, expiry, account
ID after claim, and approved ciphertext. It never receives the QR-only secret,
recipient private key, privacy key, or privacy passphrase. Poll, consume, and
cancel requests send the raw recipient token in a request header; the server
stores only its hash. The server created the fresh JWT and therefore knows that
token. It can drop or overwrite the encrypted transfer, but it cannot decrypt
it or forge a valid replacement without the QR-only secret.

Sessions expire after two minutes, and a background task normally deletes them
within another 30 seconds. The server permits at most 10,000 active pairing
sessions globally and five claimed sessions per account. Authenticated pairing
requests are limited to 120 per account per minute, while anonymous creation
uses the server's address-based request limiter. Claim and approval accept an
identical retry after success. Consumption atomically returns and deletes the
ciphertext, activates the provisional account session, and cancellation or
expiry deletes it.

The endpoints are:

- anonymous `POST /v1/pairing` to create a session with a recipient-token hash
- recipient-token `GET /v1/pairing/{id}` to inspect its metadata and state
- authenticated `POST /v1/pairing/{id}/claim` to bind it to an account, create its provisional session, and mint a JWT
- authenticated `PUT /v1/pairing/{id}` to approve it with an opaque ciphertext
- recipient-token `POST /v1/pairing/{id}/consume` to atomically consume it
- recipient-token `DELETE /v1/pairing/{id}` to cancel it

The protocol, threat model, fixed serialization, and interoperability vector
live in the OpenTubeX client repository at `docs/sync-key-pairing-v1.md`.

## Development

### Running

- Copy `config.dev.toml` to `config.toml`.
- Replace `secret_key` with the output of `openssl rand -hex 32`.
- Execute `cargo run`.
- Visit <http://localhost:8080/docs> to open the API playground.

### Adding New Database Objects or Altering Tables
+ Create a new migration with `MIGRATION_DIRECTORY=migrations/<database_backend> diesel migration generate <migration_name>` for every database backend.
+ Edit the `up.sql` and `down.sql` files in `migrations/<database_backend>/..._<migration_name>`. E.g., add a `SQL CREATE TABLE` statement or alter an existing table by adding a new field.
+ Manually create Rust structs for it in `src/models.rs`.

For more information, see <https://diesel.rs/guides/getting-started>.
