# PMO

A headless project management API: **projects, milestones and issues** (bugs, ideas, abuse reports) for client apps
and AI agents. There is no frontend. Apps build their own thin clients, and agents read the OpenAPI document.

Rust · Axum · SQLx · Postgres 17 (`pg_trgm` for typo-tolerant search) · a 15 MB distroless image · ~10 MB RAM.

- **API docs:** `https://<API_HOST>/` (→ `/docs`) · **OpenAPI:** `https://<API_HOST>/openapi.json`
- **For agents building an app's report page:** the [`pmo-thin-client` skill](.claude/skills/pmo-thin-client/SKILL.md),
  also served at `https://<API_HOST>/skills/pmo-thin-client/SKILL.md`
- **First client:** [dobre-mleko.app](https://dobre-mleko.app): a report form plus a list of open issues, so users can
  check what is already reported before filing a duplicate.

## Concepts

| | |
|---|---|
| **Users** | `admin` (manages everything, triages issues) or `reporter` (reports issues, edits the text of their own). |
| **Apps** | Client apps such as `dobre-mleko.app`. They call PMO **server-to-server** with an app key and only reach their own projects. |
| **Keys** | `Authorization: Bearer <key>`. User keys are `pmo_u_…`, app keys `pmo_a_…`. Only their SHA-256 is stored. Anyone can create more keys and revoke their own. Each key is rate limited (`RATE_LIMIT_PER_MINUTE`). App keys are server-to-server only: PMO sends no CORS headers, so browsers cannot call it, and it refuses app keys on requests with an `Origin` header. |
| **Projects** | Have a short key (`DM`) that prefixes issue keys (`DM-12`), and say what apps offer in their report forms: `platforms` and `issue_types`, each `[{id, label}]` (labels in the project's language). Reporters can only choose the project's issue types; admins can use any. |
| **Milestones** | Belong to a project; `open`/`closed`, optional due date. A project has **at most one open milestone**, and every new issue goes into it automatically (admins can pick another). To start the next one, close the current one first. |
| **Issues** | `type` bug/idea/abuse/todo · `priority` P1–P5 (default P3, set by admins) · `status` reported → accepted → in_progress → in_review → waiting_for_release → done, or wont_fix / duplicate (with `duplicate_of`); the first five are "open" · `platform`, one of the project's. |
| **Comments** | On issues: anyone who sees the issue reads its public comments and can add one; admins can also write `internal` ones (admins only). A status change can carry a comment (`PATCH /v1/issues/{key}` with `comment`), e.g. why an issue won't be fixed; the reporter's status e-mail includes it, and they are e-mailed about other people's public comments too. |
| **Attachments** | Screenshots and recordings on issues: images (PNG, JPEG, GIF, WebP, HEIC, AVIF) and videos (MP4, MOV, WebM), the type detected from the contents, up to `MAX_UPLOAD_MB` each and 20 per issue. Stored on a volume under their SHA-256. Visible to whoever sees the issue; downloads support `Range`. |
| **Action log** | Every change is logged: who (user, app or `system`), what (`issue.updated`, `attachment.added`, `key.revoked`, …), when, and the changed fields as `{"from", "to"}`. It outlives what it describes. |

**Reporters are created on the fly.** When an app reports an issue for an e-mail that has no account yet
(`"reporter": {"email": …, "name": …}`), PMO creates a reporter and e-mails them a personal key. They can use that
key with other apps or their own AI agents. Apps must therefore only pass e-mails their own users have proven, such
as from a Google sign-in. The reporter is also e-mailed whenever an admin changes the status of their issue.

**Visibility.** Admins see everything. **Abuse reports are always created private**, and any issue can be made
private. Private issues are visible only to admins and their reporter, never to apps. Reporter e-mails are shown
only to admins and the reporter themselves.

### Endpoints

| | |
|---|---|
| `GET /v1/me` · `GET/POST /v1/me/keys` · `DELETE /v1/keys/{id}` | The caller and their keys |
| `GET/POST /v1/projects` · `GET/PATCH /v1/projects/{key}` | Projects (writes: admin) |
| `GET/POST /v1/projects/{key}/milestones` · `GET/PATCH/DELETE /v1/milestones/{id}` | Milestones (writes: admin) |
| `GET/POST /v1/projects/{key}/issues` · `GET/PATCH/DELETE /v1/issues/{key}` | Issues |
| `GET/POST /v1/issues/{key}/comments` · `PATCH/DELETE /v1/comments/{id}` | Comments (edit and delete: the author or admins) |
| `GET/POST /v1/issues/{key}/attachments` · `GET/DELETE /v1/attachments/{id}` | Attachments (upload: multipart `file`) |
| `GET /v1/issues/{key}/events` · `GET /v1/events` | An issue's history; the whole log (admin, filterable by project, issue, action, actor, time) |
| `/v1/users…` · `/v1/apps…` | Admin only: users, apps and their keys |

Listing issues takes `status` (comma-separated, or `open` / `closed`), `type`, `priority`, `platform`,
`milestone_id`, `reporter_id`, `q` (search, sorted by relevance), `sort` (`newest`, `oldest`, `priority`, `updated`), `limit` and
`offset`. Errors are always `{"error": "not_found", "message": "…"}`.

## Quick start

You need Rust (stable) and Postgres. `docker compose up -d` starts Postgres and [Mailpit](http://localhost:8025),
which catches outgoing e-mails.

```sh
cp .env.example .env && set -a && . ./.env && set +a
cargo run -- create-key you@example.com --admin     # prints an admin key
cargo run                                           # http://localhost:8080/docs
```

```sh
KEY=pmo_u_…
curl -s localhost:8080/v1/projects -H "authorization: Bearer $KEY" -H 'content-type: application/json' \
  -d '{"key": "DM", "name": "Dobre Mleko"}'
```

### Commands

| Command | What it does |
|---|---|
| `pmo` / `pmo serve` | Run the API. Applies migrations and makes `ADMIN_EMAILS` admins at startup. |
| `pmo create-key <email> [--admin]` | Print a new key for a user, creating the user if needed |
| `pmo migrate` | Apply migrations only |
| `pmo openapi` | Print the OpenAPI document |

### Tests

Tests run against a real Postgres. `#[sqlx::test]` creates a fresh database per test, so the role needs `CREATEDB`:

```sh
DATABASE_URL=postgres://pmo:pmo@localhost:5432/pmo cargo test
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

## Configuration

| Variable | |
|---|---|
| `DATABASE_URL` | Required |
| `PORT` | Default `8080` |
| `PUBLIC_URL` | Public base URL, used in e-mails and the OpenAPI document |
| `ADMIN_EMAILS` | Comma-separated. These users become admins at startup; an admin who never had a key gets one by e-mail. |
| `SMTP_HOST`, `SMTP_PORT`, `SMTP_USERNAME`, `SMTP_PASSWORD` | E-mail server. Port `465` (default) uses TLS, other ports STARTTLS. |
| `SMTP_URL` | Alternative to the above, e.g. `smtp://localhost:1025` for Mailpit |
| `MAIL_FROM` | Default `PMO <SMTP_USERNAME>` |
| `STORAGE_DIR` | Attachment files, default `./data/attachments` (a volume in production) |
| `MAX_UPLOAD_MB` | Largest attachment, default `100` |
| `RATE_LIMIT_PER_MINUTE` | Requests per key per minute (bursts up to it, refilled evenly), default `600`; `0` turns it off. Beyond it: 429 with `Retry-After` |

Without SMTP, e-mails are only logged; their bodies (which can contain keys) appear only at `RUST_LOG=debug`.

### Gmail as the SMTP server

1. Turn on 2-Step Verification for the Google account.
2. Create an app password at https://myaccount.google.com/apppasswords and use its 16 letters, without spaces, as
   `SMTP_PASSWORD`.
3. Set `SMTP_HOST=smtp.gmail.com` and `SMTP_USERNAME=you@gmail.com`. Gmail sends from that address and allows about
   500 recipients a day, which is plenty for now. Switch to a transactional provider (Resend, Postmark, SES) when
   that is no longer enough.

## Deployment

Pushes to `main` run CI. When CI passes, **Build and deploy** pushes the image to GHCR and applies `k8s.yaml` to k3s
over SSH (namespace `pmo`: Postgres StatefulSet, API Deployment with a 10 Gi attachments volume, Traefik Ingress
with a cert-manager certificate). The attachments volume is ReadWriteOnce, so deploys use the `Recreate` strategy:
a few seconds of downtime. Back up both the Postgres volume and the attachments volume.

Repository settings → Secrets and variables → Actions:

| Secret | |
|---|---|
| `VPS_HOST`, `VPS_SSH_KEY` | As for the other projects |
| `POSTGRES_PASSWORD` | e.g. `openssl rand -hex 24`. It is set when the database is first created, so don't change it later. |
| `ADMIN_EMAILS` | Your e-mail (kept as a secret so it stays out of this public repository) |
| `SMTP_PASSWORD` | Gmail app password. Empty turns e-mail off. |

| Variable | |
|---|---|
| `API_HOST` | e.g. `pmo.walczaki.com`, with DNS pointing at the VPS |
| `SMTP_USERNAME` | e.g. `you@gmail.com` |
| `SMTP_HOST`, `MAIL_FROM` | Optional; default `smtp.gmail.com` and `PMO <SMTP_USERNAME>` |

The first deploy creates your admin account and e-mails you its key. Without e-mail, create one on the server:

```sh
kubectl -n pmo exec deploy/api -- /pmo create-key you@example.com --admin
```

If the pod cannot pull the image, make the `pmo` package public under GitHub → Packages → pmo → Package settings.

## Onboarding a client app

With your admin key:

```sh
API=https://pmo.walczaki.com; H="authorization: Bearer $ADMIN_KEY"
curl -s $API/v1/projects -H "$H" -H 'content-type: application/json' -d '{"key": "DM", "name": "Dobre Mleko"}'
curl -s $API/v1/apps -H "$H" -H 'content-type: application/json' -d '{"name": "dobre-mleko.app", "projects": ["DM"]}'
# → { "app": {…}, "key": { "key": "pmo_a_…", … } }  Put that key in the app's *server-side* configuration.
```

The app's backend then:

```sh
# Report form
curl -s $API/v1/projects/DM/issues -H "authorization: Bearer $APP_KEY" -H 'content-type: application/json' \
  -d '{"type": "bug", "title": "Map does not load", "description": "…", "reporter": {"email": "ola@example.com", "name": "Ola"}}'
# "Already reported?" while the user types, and the list of open issues
curl -s "$API/v1/projects/DM/issues?status=open&q=map" -H "authorization: Bearer $APP_KEY"
```

To rotate an app key: `POST /v1/apps/{id}/keys`, deploy the new key, then `DELETE /v1/keys/{old id}`.
