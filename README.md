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

### Deprecated playback-speed API

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

### Configuration Reference:

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

## API Documentation
- Start the app, e.g. with `cargo run`.
- The documentation can now be found at `http://localhost:8080/docs`.

### Authentication
After registering or logging in, you receive a `jwt` as response.

This `jwt` must be passed either as `Authorization` cookie or header for authenticated requests, e.g. for creating subscriptions.
For example:
- Header: `Authorization: abcdefghijklmnopqrtuvwxyz`
- Cookie: `Authorization=abcdefghijklmnopqrtuvwxyz`

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
for profiles, playback speeds, and sessions, 16 MiB for subscriptions and playlist
bookmarks, and 64 MiB for playlists and history. The combined active encrypted
collections for one account cannot exceed 128 MiB.

## Developing
### Adding New Database Objects or Altering Tables
+ Create a new migration with `diesel migration generate <migration_name>` 
+ Edit the `up.sql` and `down.sql` files in `migrations/..._<migration_name>`. E.g., add a `SQL CREATE TABLE` statement or alter an existing table by adding a new field.
+ Manually create Rust structs for it in `src/models.rs`.

For more information, see <https://diesel.rs/guides/getting-started>.
