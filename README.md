# OpenTubeX Sync Server

Keeps OpenTubeX data in sync across devices: subscriptions, playlists, watch
history, profiles, sessions, and settings.

Based on the [LibreTube sync server](https://github.com/libre-tube/sync-server).

## Privacy

OpenTubeX clients encrypt everything they sync with a separate privacy
passphrase before uploading it. The server stores each collection as an opaque,
revisioned document.

| The server can see                                              | The server cannot see                          |
| --------------------------------------------------------------- | ---------------------------------------------- |
| Account activity and request timing                             | Synced data, such as subscriptions and history |
| Client IP addresses                                             | Device names and platforms                     |
| Collection names and approximate size (padded to 64 KiB blocks) | The privacy passphrase or key                  |

Older clients can still use the LibreTube-compatible plaintext endpoints. Once
an account uploads its first encrypted collection:

- the server deletes that account's plaintext sync records (existing server
  backups keep them)
- plaintext sync endpoints reject the account, so an older client cannot
  re-upload readable data

The public instance's privacy policy is in [PRIVACY.md](./PRIVACY.md).

## Self-hosting

### Quick start

1. Download [docker-compose.yml](./docker-compose.yml) (SQLite) or
   [docker-compose.postgres.yml](./docker-compose.postgres.yml) (PostgreSQL).
   Both images support ARM64 and x86:

   | Image                                           | Database   |
   | ----------------------------------------------- | ---------- |
   | `ghcr.io/opentubex/sync-server:latest-sqlite`   | SQLite     |
   | `ghcr.io/opentubex/sync-server:latest-postgres` | PostgreSQL |

2. Create a `.env` file next to it. Use the same value for both secrets (see
   [Secrets](#secrets)):

   ```sh
   secret=$(openssl rand -hex 32)
   printf 'SECRET_KEY=%s\nUSERNAME_SECRET=%s\n' "$secret" "$secret" > .env
   # PostgreSQL only:
   echo "POSTGRES_PASSWORD=$(openssl rand -hex 32)" >> .env
   ```

3. SQLite only: let the container user (uid `10001`) write the data directory
   (see [File permissions](#file-permissions)):

   ```sh
   mkdir -p ./data && sudo chown -R 10001:10001 ./data
   ```

4. Start the server with `docker compose up -d`. It listens on
   `127.0.0.1:8080`.
5. Point a reverse proxy at it. See [Reverse proxy](#reverse-proxy).

### Configuration

Set options in a `config.toml` in the server's working directory, or as
environment variables (case-insensitive, e.g. `ALLOW_REGISTRATION=false`).
Environment variables override `config.toml`. For options inside a section, join
the names with `__`, e.g. `OIDC__CLIENT_ID` for `client_id` in `[oidc]`.
[config.dev.toml](./config.dev.toml) is an annotated example.

| Option                        | Description                                                                          | Default      |
| ----------------------------- | ------------------------------------------------------------------------------------ | ------------ |
| `database_url`                | Database connection string, e.g. `./data/db.sqlite`                                  | required     |
| `secret_key`                  | Signs authentication tokens. At least 32 bytes                                       | required     |
| `username_secret`             | Derives account name hashes. See [Secrets](#secrets)                                 | `secret_key` |
| `trust_forwarded_for`         | Take client addresses from `X-Forwarded-For`. See [Reverse proxy](#reverse-proxy)    | `false`      |
| `trusted_proxy_hops`          | Number of proxies in front of the server                                             | `1`          |
| `allow_registration`          | Allow new accounts, including first-time OIDC sign-ins                               | `true`       |
| `validate_submitted_metadata` | Check submitted channel and video metadata against YouTube                           | `true`       |
| `privacy_policy_url`          | HTTP(S) URL without credentials, shown to clients through `/health`                  | none         |
| `migration_approval`          | Pending migration versions to apply. See [Database migrations](#database-migrations) | none         |
| `oidc`                        | OpenID Connect login. See [OIDC](#oidc)                                              | disabled     |

### Secrets

- The server refuses to start if `secret_key` is missing, shorter than 32
  bytes, or a placeholder such as `changeme`.
- Accounts are looked up by `HMAC(username)`. Changing the secret behind that
  hash locks every account out.
- Set `username_secret` to the initial `secret_key` value. You can then rotate
  `secret_key` later, for example after a leak, without locking anyone out.
  Never change `username_secret`.

### Reverse proxy

Registration and login are rate limited per client address. Behind a reverse
proxy, every request comes from the proxy, so all clients would share one
limit. Configure the server to read the real address from `X-Forwarded-For`:

| Setup                                     | `trust_forwarded_for` | `trusted_proxy_hops` |
| ----------------------------------------- | --------------------- | -------------------- |
| Server directly reachable, no proxy       | `false`               | —                    |
| One reverse proxy (example compose files) | `true`                | `1`                  |
| Cloudflare Tunnel (`cloudflared`)         | `true`                | `2`                  |
| Other proxy chains                        | `true`                | number of proxies    |

- The server counts `trusted_proxy_hops` entries from the end of the header.
  Your own proxies appended those, so clients cannot pick their address by
  sending the header themselves.
- Only enable `trust_forwarded_for` when the server cannot be reached directly.
  Otherwise clients can spoof their address.
- A wrong hop count is worse than none. With Cloudflare Tunnel at `1`, the
  address changes between Cloudflare edge servers. Clients can then bypass
  the limit, and others get rate limited at random.
- To check your setup, send more than 10 login requests within a minute from
  one client. Responses should switch to `429` and stay there. If they alternate,
  the hop count is wrong.
- Access logs use the same client address. They fall back to the peer address
  when the header is untrusted, missing, or invalid.
- The limiter is per process and best effort. Rate limit at the proxy as well,
  which is required with multiple replicas.
- Every `/v1/...` path also works without the `/v1` prefix. Proxy or WAF rules
  for `/v1/account/delete` must also cover `/account/delete`.

### File permissions

The image runs as uid `10001`, but a bind-mounted directory keeps its host
ownership. SQLite deployments need a data directory that uid can write:

```sh
mkdir -p ./data && sudo chown -R 10001:10001 ./data
```

- Otherwise the server fails with a database connection timeout, not a
  permission error.
- Upgrading from an image that ran as root needs the same command. It only
  changes ownership, not data.
- Without `sudo`, run the container as the directory's owner instead:

  ```yaml
  services:
    sync:
      user: "1000:1000" # from `id -u`:`id -g`
  ```

### Database migrations

The server never migrates an existing database on its own. To upgrade:

1. Back up the database and verify the backup.
2. Start the new version. It refuses to start and logs the pending migration
   versions.
3. Set `migration_approval` to exactly those versions, comma-separated (for
   example `MIGRATION_APPROVAL=202607211800000000`), and start it again.
   SQLite also writes a `*.pre-migration-<version>` backup before migrating.
4. Remove `migration_approval` so it cannot approve a later migration.

### OIDC

Add an `[oidc]` section to `config.toml`, or set the matching `OIDC__*`
environment variables, to let users sign in through an OpenID Connect provider.
All options except `allowed_redirect_urls` and `display_name` are required. As an environment
variable, `allowed_redirect_urls` takes a comma-separated list, e.g.
`OIDC__ALLOWED_REDIRECT_URLS=myapp://oidc,https://app.example.com/oidc`.
With the Compose quick start, add the variables to the service's `environment`
list. Compose doesn't pass entries in `.env` to the container on its own.

| Option                  | Description                                              | Example                    |
| ----------------------- | -------------------------------------------------------- | -------------------------- |
| `provider_url`          | Base URL of the OIDC provider                            | `https://auth.example.com` |
| `client_id`             | Client ID of the OAuth app                               |                            |
| `client_secret`         | Client secret of the OAuth app                           |                            |
| `app_url`               | Public URL of this server                                | `https://sync.example.com` |
| `allowed_redirect_urls` | Client URLs that login and deletion may redirect back to | `["opentubex://oidc"]`     |
| `display_name`          | Provider name that clients show on their login button    | `Authelia`                 |

- Allow these redirect URLs in the OAuth app:
  - `<app_url>/v1/account/oidc/authenticate/callback`
  - `<app_url>/v1/account/oidc/delete/callback`
- OIDC login hands the session token to the client's redirect URL. The server
  only accepts URLs listed exactly in `allowed_redirect_urls`, and refuses OIDC
  login and deletion while the list is empty.
- With `allow_registration = false`, users cannot sign in with OIDC for the
  first time.
- The OpenTubeX desktop and mobile apps return to `opentubex://oidc`. They show
  the login button when `GET /meta` reports `"oidc": true`, labeled with
  `display_name` if set.

### External services

- **YouTube**, to validate submitted channel and video metadata. Disable with
  `validate_submitted_metadata = false`.
- **[Have I Been Pwned](https://haveibeenpwned.com/Passwords)**, to reject
  breached passwords at registration and password changes. Only the first five
  hex digits of the password's SHA-1 hash are sent. If the lookup fails, the
  password is accepted.

## API

Every instance serves interactive API documentation at `/docs`. It lists the
canonical `/v1` paths. Each one also works without the prefix, using the same
handler, and unprefixed paths remain version 1 aliases if a later version is
added.

### Authentication

Register or log in with a username and password, or through [OIDC](#oidc). The
response contains a `jwt`. Send it as a header or cookie on authenticated
requests:

```http
Authorization: <jwt>
Cookie: Authorization=<jwt>
```

To delete an OIDC account, send an authenticated `POST /v1/account/oidc/delete`
and open the returned `authorization_url` in a browser. The account is deleted
once its owner signs in there.

### Capabilities

`GET /health` returns the server status, its capabilities, and
`privacy_policy_url` if configured. Clients use the capabilities to decide which
features they can use.

| Capability                                                                                                                            | Meaning                                                        |
| ------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| `encrypted_sync`                                                                                                                      | [Encrypted sync](#encrypted-sync) is available                 |
| `collection_changes`                                                                                                                  | Collections accept [incremental changes](#incremental-changes) |
| `live_sync`                                                                                                                           | [Change notifications and device requests](#live-sync)         |
| `account_sessions`                                                                                                                    | Tokens are bound to [account sessions](#account-sessions)      |
| `key_pairing`                                                                                                                         | [Secure device pairing](#secure-device-pairing)                |
| `bulk_sync`                                                                                                                           | Plaintext subscriptions and watch history accept bulk uploads  |
| `history_page_size`                                                                                                                   | Maximum page size for plaintext watch history                  |
| `seen_videos`, `seen_posts`, `seen_rss_items`, `watch_stats`, `live_reminders`, `external_media_positions`, `notification_dismissals` | The matching [collection](#collections) exists                 |

### Encrypted sync

| Endpoint                              | Purpose                               |
| ------------------------------------- | ------------------------------------- |
| `GET /v1/encrypted_sync`              | Manifest of collections and revisions |
| `GET /v1/encrypted_sync/{collection}` | Download one collection               |
| `PUT /v1/encrypted_sync/{collection}` | Upload one collection                 |

- Each collection has its own revision. Uploading with a stale revision fails
  with `409` without blocking other collections.
- While an account still has plaintext records, the manifest sets
  `legacy_data`. The client then pulls and merges them before its first
  encrypted upload.
- Each plaintext domain is deleted in the same transaction that stores its
  encrypted collection, so an interrupted migration can resume.

#### Collections

| Collection                                                | Size limit | Capability                 |
| --------------------------------------------------------- | ---------- | -------------------------- |
| `settings`                                                | 2 MiB      |                            |
| `notificationDismissals` (read and removed notifications) | 2 MiB      | `notification_dismissals`  |
| `profiles`, `sessions`, `sessionsV2`                      | 8 MiB      |                            |
| `watchStats` (per-device totals)                          | 8 MiB      | `watch_stats`              |
| `liveReminders`                                           | 8 MiB      | `live_reminders`           |
| `subscriptions`, `playlistBookmarks`                      | 16 MiB     |                            |
| `seenVideos`                                              | 16 MiB     | `seen_videos`              |
| `seenPosts`                                               | 16 MiB     | `seen_posts`               |
| `seenRssItems`                                            | 16 MiB     | `seen_rss_items`           |
| `externalMediaPositions` (media opened by URL)            | 16 MiB     | `external_media_positions` |
| `playlists`, `history`                                    | 64 MiB     |                            |

All collections of an account, including pending changes, are limited to
128 MiB in total. `seenPosts` is separate from `seenVideos` so older clients
that sync `seenVideos` cannot erase post marks.

### Incremental changes

With `collection_changes`, clients can upload a small encrypted change instead
of the whole collection. The server stores changes as opaque ciphertext, so
their format is up to the clients.

- `POST /v1/encrypted_sync/{collection}/changes` takes the same body as `PUT`
  and appends one change at the next revision. Each change can be up to
  256 KiB.
- `GET /v1/encrypted_sync/{collection}?since={revision}` returns
  `base_revision` and `changes`:
  - If the change log covers `since`, the response omits `payload` and lists
    only later changes.
  - Otherwise it contains the full `payload` at `base_revision`, followed by
    every change after it.
- `PUT` stores a full payload and clears the change log.
- While a collection has pending changes, reads without `since` fail with
  `409`, because those clients cannot apply changes.

An append fails with `412`, and the client uploads the full collection instead,
when:

- any session of the account that was active in the last 30 days has never
  read with `since` or appended a change. This includes an older client
  signing in, or using a legacy token for the first time.
- the collection has never been fully uploaded
- the collection already has 64 pending changes
- the pending changes would outgrow the full payload (minimum 1 MiB)

### Live sync

With `live_sync`:

- `GET /v1/encrypted_sync/changes?since={cursor}` returns the current cursor
  once it differs from `since`, waiting up to 25 seconds for a change.
- `GET /v1/encrypted_sync/events?since={id}` lists encrypted events for the
  requesting device: activity records that clients attach to uploads, and
  requests from other devices, such as opening a video.
- `POST /v1/encrypted_sync/events` sends an encrypted request to another
  device, and `DELETE /v1/encrypted_sync/events/{id}` acknowledges one.
- Event payloads are limited to 256 KiB, and an account can have up to 100
  pending device requests.

### Account sessions

With `account_sessions`, each token belongs to a stored session, named in its
`jti` claim. Revoked or expired sessions stop authenticating immediately.

| Endpoint                           | Purpose                                                                   |
| ---------------------------------- | ------------------------------------------------------------------------- |
| `GET /v1/account/sessions`         | List active sessions, and whether the account supports password login     |
| `PATCH /v1/account/sessions/{id}`  | Store encrypted device information (`current` for the requesting session) |
| `DELETE /v1/account/sessions/{id}` | Revoke a session                                                          |
| `PUT /v1/account/password`         | Change the password and revoke all other sessions                         |

- **Creation.** Registration, password login, and OIDC login create an active
  session. [Pairing](#secure-device-pairing) creates a provisional one that
  becomes active once the new device consumes the pairing payload.
- **Devices.** Clients send a random 16-byte base64url device ID when they
  authenticate. Older clients that omit it get a server-assigned one.
- **Device information.** The device name, operating system, release, and
  architecture are encrypted with the privacy key. Any active device can
  replace them, for example to rename another device.
- **Timestamps.** The server keeps creation, last-active, and expiry times.
  Last-active is written at most every five minutes. Expired and revoked
  sessions are removed within about an hour of expiring.
- **Password changes** verify the current password, return a new token for the
  requesting device, and revoke every other session in one transaction. A login
  that checked the old password concurrently is rejected too.

Tokens issued before account sessions have no `jti`:

- Accounts that existed at the migration can keep using such a token until it
  expires. Its session is created on first use. New accounts need
  session-bound tokens.
- The session and device IDs are derived from a SHA-256 digest of the token.
  The token itself is not stored.
- A revoked legacy session leaves a tombstone until the token expires, so the
  token cannot recreate it.
- Password changes reject legacy tokens that have not created a session yet.

### Secure device pairing

With `key_pairing`, a signed-in device can set up a new device for encrypted
sync without the password or passphrase:

1. The new device creates a pairing session and shows a QR or text code.
2. A signed-in device scans it and claims the session. The server creates a
   provisional account session and a token for the new device.
3. The signed-in device encrypts the token, account name, privacy key, privacy
   salt, and a six-digit verification code, and uploads them.
4. The new device consumes the payload. This deletes it and activates the
   session.

| Endpoint                        | Authentication  | Purpose                                                   |
| ------------------------------- | --------------- | --------------------------------------------------------- |
| `POST /v1/pairing`              | none            | Create a session with a recipient-token hash              |
| `GET /v1/pairing/{id}`          | recipient token | Read its metadata and state                               |
| `POST /v1/pairing/{id}/claim`   | account         | Bind it to the account and create the provisional session |
| `PUT /v1/pairing/{id}`          | account         | Approve it with the encrypted payload                     |
| `POST /v1/pairing/{id}/consume` | recipient token | Return and delete the payload                             |
| `DELETE /v1/pairing/{id}`       | recipient token | Cancel it                                                 |

**What the server knows.** It stores the session ID, a SHA-256 hash of the
recipient token, the recipient public key, device IDs, the new device's display
name, the expiry, the account ID after the claim, and the encrypted payload. It
never receives the QR-only secret, the recipient private key, the privacy key,
or the passphrase. It knows the new token because it created it. It can drop or
overwrite the payload, but cannot decrypt it or forge a valid one.

**Limits.**

- Sessions expire after two minutes and are usually deleted within another
  30 seconds. Cancelling deletes them right away.
- At most 10,000 active sessions server-wide, and five claimed sessions per
  account.
- Authenticated pairing requests are limited to 120 per account per minute.
  Creation uses the per-address rate limit.
- Repeating a successful claim or approval with the same data succeeds again.

The protocol, threat model, serialization, and test vector are documented in
the OpenTubeX client repository at `docs/sync-key-pairing-v1.md`.

## Development

1. Copy `config.dev.toml` to `config.toml`.
2. Set `secret_key` to the output of `openssl rand -hex 32`.
3. Run `cargo run`.
4. Open <http://localhost:8080/docs> for the API playground.

### Database changes

1. Generate a migration for each backend:
   `MIGRATION_DIRECTORY=migrations/<backend> diesel migration generate <name>`.
2. Edit `up.sql` and `down.sql` in `migrations/<backend>/..._<name>`.
3. Add the matching Rust structs to `src/models.rs`.

See the [Diesel guide](https://diesel.rs/guides/getting-started) for details.
