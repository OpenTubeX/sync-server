# OpenTubeX Sync Server Privacy Policy

Last updated: August 26, 2026

This policy applies to the public OpenTubeX sync server at
[sync.d3sox.me](https://sync.d3sox.me). Other operators running this
open-source software are responsible for their own privacy notices.

## Operator

- Controller: [D3SOX](https://github.com/D3SOX)
- Privacy contact: [privacy@opentubex.org](mailto:privacy@opentubex.org)

The operator is responsible for the data described below. The OpenTubeX project
does not control data processed by independently hosted instances.

## Data we process

**Account data.** The server stores a unique account ID, a deterministic
HMAC-derived value of your account name, and a salted Argon2 password hash. It
does not store your account name or password in plaintext. It issues a signed
authentication token after login but does not store that token in the
database.

**Encrypted sync.** The public server supports encrypted sync, and OpenTubeX
always uses it when the server supports it. Sync data is encrypted on your
device using a separate privacy passphrase. The server stores the encrypted
payload, collection name, revision, account ID, and padded payload size. It
never receives the passphrase or plaintext. The operator can still observe
account activity, request timing, collection names, and approximate data size.
The server cannot recover a lost privacy passphrase.

**Device pairing.** Secure device pairing temporarily stores a one-time session
ID, SHA-256 recipient-token hash, recipient public key, pairing-scoped device
identifiers, the receiving device's user-chosen display name, expiry time, and
an encrypted pairing payload. It adds the account ID when an authenticated
device claims the session. Sessions expire after two minutes and are deleted
when they are consumed or cancelled. Poll, consume, and cancel requests send
the raw recipient token in a request header; the server stores only its hash.
The server never receives the QR-only pairing secret, recipient private key,
privacy key, privacy passphrase, or login password. It creates a fresh
authentication token for the receiving device during the claim and therefore
knows that token. The approving device places the token inside the encrypted
relay payload together with the account name, privacy key, privacy salt, and
six-digit verification code. The server can drop or overwrite that ciphertext,
but it cannot decrypt it or forge a valid replacement without the QR-only
secret. A background task deletes expired sessions, normally within 30 seconds
after their two-minute expiry.

**Legacy compatibility.** The server retains plaintext endpoints for older or
non-OpenTubeX clients. Current OpenTubeX clients do not use them on this public
server. A client using these endpoints may send readable subscriptions, groups,
playlists and bookmarks, watch history and progress, playback speeds, and
related public YouTube metadata. Uploading the corresponding encrypted
collection removes account-linked legacy data from the active database, but
not immediately from existing backups.

**Request logs.** The default logger may record IP address, time, method, path,
response status and size, duration, user agent, and referrer. Hosting, proxy,
security, or backup systems may process the same connection data.

The server contains no advertising or analytics trackers.

## Why we process it

We process this data to create and authenticate accounts, synchronize data
between your devices, resolve update conflicts, enforce storage limits, operate
and secure the service, diagnose errors or abuse, and comply with legal
obligations.

Where the GDPR applies, providing the requested sync service is based on
Article 6(1)(b). Security, abuse prevention, and reliable operation are based on
the operator's legitimate interests under Article 6(1)(f). Processing required
by law is based on Article 6(1)(c).

We do not sell personal data, use it for advertising, or make automated
decisions with legal or similarly significant effects.

## Sharing and transfers

The application and database run on a privately managed server. Cloudflare,
Inc. provides DNS, reverse-proxy, DDoS protection, and tunnel services through
its global network. Cloudflare therefore processes connection information and
HTTP traffic as a service provider; encrypted sync payloads remain ciphertext.
Its policy states that information is stored primarily in the United States
and the EEA and may be processed globally.

For transfers from the EEA, Cloudflare relies on the EU-U.S. Data Privacy
Framework and, where needed, the European Commission's Standard Contractual
Clauses. Details are available in Cloudflare's
[privacy policy](https://www.cloudflare.com/policies/privacy/) and
[Data Processing Addendum](https://www.cloudflare.com/cloudflare-customer-dpa/).

We may disclose data when required by applicable law or a binding legal
request.

## Retention and deletion

Account and sync data remain in the active database until you delete individual
items or your account. Account deletion removes the account and its linked data
from the active database. Shared public YouTube metadata may remain.

- Request logs are retained for up to seven days.
- A database backup is created daily, with the seven most recent backups
  retained. Temporary migration or restore safety copies are retained for up
  to 30 days.

Deleted data may remain in these backups until the applicable retention period
ends, or longer when required by law.

## Security

The server uses password hashing and signed authentication tokens, and supports
client-side encryption. Operators must use HTTPS and protect signing keys,
databases, logs, and backups. No system can guarantee absolute security.
Client-side encryption does not hide connection metadata, timing, collection
names, or approximate data size, and it does not protect legacy sync data.

## Your rights

Depending on applicable law, you may request access, correction, deletion,
restriction, objection, or portability. You may also complain to your local
data protection authority. Contact the operator using the details listed above;
the operator may need to verify that you control the account.

You can also delete your account through the authenticated account-deletion
endpoint. To avoid exposing sensitive information, do not post passwords,
authentication tokens, or sync data in public GitHub issues.

We may update this policy when the service or its data practices change. The
date at the top identifies the latest version.
