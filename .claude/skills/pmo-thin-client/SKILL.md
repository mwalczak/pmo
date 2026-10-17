---
name: pmo-thin-client
description: Add issue reporting (bug reports, ideas, abuse reports with screenshots and recordings) to an app, backed by PMO. Use when building or changing an app's "report a problem" page or its backend proxy to PMO, or when connecting a new app to PMO. Covers the server-side proxy, the report form, the "already reported" list, attachments, privacy and tests, following the dobre-mleko.app integration.
---

# Building a thin PMO client

PMO is a headless issue tracker (API docs: `https://pmo.walczaki.com/docs`, OpenAPI: `/openapi.json`). Apps don't
build a tracker. Each app adds a thin layer: a **backend proxy** holding the app's PMO key, and a **report page**
that reads everything it offers from PMO. The reference implementation is dobre-mleko:
`mwalczak/dobre-mleko-be` (`src/pmo.ts`, `src/routes/reports.ts`, `test/reports.test.ts`) and
`mwalczak/dobre-mleko-web` (`src/app/pages/ReportPage.tsx`, `src/app/reports.ts`).

## Rules

1. **The app key never reaches a client.** App keys (`pmo_a_…`) are server-to-server only. The browser or mobile app
   calls the app's own backend; only the backend calls PMO. Read the key from configuration (`PMO_API_KEY`), never
   from source.
2. **Report as the signed-in user.** The backend sends `reporter: {email, name}` from its own session, and only an
   e-mail the user proved (e.g. a Google sign-in). Never take the reporter e-mail from the request body. PMO creates
   an account for a new reporter and e-mails them a personal key, then e-mails them status changes.
3. **Nothing about the form is hardcoded.** Issue types (with labels in the project's language) and platforms come
   from `GET /v1/projects/{key}` → `issue_types: [{id, label}]`, `platforms: [{id, label}]`. Show the labels as they
   are. Statuses are a fixed set (`reported`, `accepted`, `in_progress`, `done`, `wont_fix`, `duplicate`); translate
   those in the app.
4. **Abuse reports are private.** PMO never lists them to apps. Don't offer "similar reports" for them, and tell
   the user only admins will see the report.
5. **Fail soft.** Without configuration, or when PMO is down, the proxy answers 503 (`reports_unavailable`) and the
   page says reporting is unavailable. A PMO 400 becomes the app's validation error (422).

## Setup (ask the PMO admin)

- A project (key such as `DM`) configured with `issue_types` and `platforms` (`PATCH /v1/projects/{key}`).
- An app with access to that project: `POST /v1/apps {"name": "<domain>", "projects": ["DM"]}` returns the key once.
- In the app backend's configuration: `PMO_API_URL=https://pmo.walczaki.com`, `PMO_API_KEY` (a secret, also in the
  deploy secrets and the k8s Secret), `PMO_PROJECT=DM`. Empty values turn reporting off.

## Backend: four endpoints, signed-in users only

| App endpoint | Calls PMO | Notes |
|---|---|---|
| `GET /reports/options` | `GET /v1/projects/{key}` | Return `{issue_types, platforms}`. Cache for a few minutes. |
| `GET /reports?status=open\|closed&q=&limit=&offset=` | `GET /v1/projects/{key}/issues` with the same query | `q` is a typo-tolerant search sorted by relevance. Pass `next_offset` through. |
| `POST /reports` (multipart: `type`, `title`, `description?`, `platform?`, `files[]`) | `POST /v1/projects/{key}/issues`, then `POST /v1/issues/{issueKey}/attachments` per file | Uploading in the same request means the app needn't track who owns which issue. If a file fails, still return the created report and say which files failed. |
| `GET /reports/attachments/{id}` (optional) | `GET /v1/attachments/{id}` | Stream it through, passing `Range` for videos. Only if the page shows attachments. |

- Send `Authorization: Bearer ${PMO_API_KEY}` and a timeout (about 10 s; uploads longer) on every call.
- Return only the fields the page needs: `key`, `type`, `status`, `priority`, `title`, `description`, `platform`,
  `private`, `duplicate_of`, `created_at`, `updated_at`. **Never pass reporter data through.**
- Attachments: images (PNG, JPEG, GIF, WebP, HEIC, AVIF) and videos (MP4, MOV, WebM), up to 100 MB each and 20 per
  issue. PMO detects the type from the contents and answers 415 or 413 otherwise. Stream uploads rather than buffering
  them, and set the app's own body limit to match.
- Validate input in the app (type in the options, title 3–200 characters, description up to about 5000) before
  calling PMO.

## Report page (e.g. `/report`, behind sign-in)

- **Type**: chips built from `options.issue_types`, first one selected.
- **Platform**: preselect the platform the client runs on (the web app `web`, the Android app `android`, iOS `ios`)
  if it is in `options.platforms`. Show the choice only if the user might report for another platform.
- **Title** (required): while the user types (debounced about 350 ms, at least 3 characters, not for private types),
  show up to 5 matches from `GET /reports?q=…&limit=5` under "Maybe this is already reported?".
- **Description**, with a placeholder per type.
- **Attachments**: an image/video picker with previews and a size check before upload. Optional.
- A note on privacy: the report goes to the tracker with the user's name and e-mail; the first time they get a key by
  e-mail; they're notified of status changes.
- **After sending**: show the new key (`DM-12`), "we'll let you know by e-mail", "Report something else", and refresh
  the list.
- **Already reported**: open/closed tabs, rows with a type mark (shape and colour, plus the label), key, date, title,
  clamped description and the status label. Offer "Show more" while `next_offset` is set. Show an empty state and an
  unavailable state.
- Link to the page from somewhere obvious (profile, settings, help).

## Privacy

Update the app's privacy policy. Reports (content, attachments, the user's name and e-mail) go to PMO, the tracker,
which e-mails the reporter. Abuse reports are visible only to admins. Reports stay after the user deletes their app
account unless they ask.

## Tests

- **Backend:** inject `fetch` into the PMO client and fake PMO in memory. Check the `Authorization` header, the
  reporter taken from the session (not the body), the query passthrough, attachments uploaded after the issue is
  created, 503 without configuration or when PMO fails, 422 on a PMO 400, and that reporter data is not returned.
- **Frontend e2e:** answer `/api/reports*` with route mocks. Cover: options render as chips, similar reports while
  typing, hidden for abuse, the submitted body, the confirmation, and the list.

## PMO calls at a glance

```http
GET  /v1/projects/DM                          → {issue_types:[{id,label}], platforms:[{id,label}], …}
GET  /v1/projects/DM/issues?status=open&q=map → {items:[Issue], next_offset}
POST /v1/projects/DM/issues                   {type, title, description, platform, reporter:{email,name}} → Issue
POST /v1/issues/DM-12/attachments             multipart "file" → Attachment {id, url, kind, content_type, size}
GET  /v1/attachments/{id}                     → the file (Range supported)
GET  /v1/issues/DM-12/events                  → history: who changed what, when
```
