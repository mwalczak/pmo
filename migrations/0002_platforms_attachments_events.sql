-- Simple things to do: not a bug, not an idea.
ALTER TYPE issue_type ADD VALUE 'todo';

-- What a project offers its reporters, so apps build their forms from it: the platforms it ships on and the issue
-- types, each as [{"id": …, "label": …}]. An issue may name one of the platforms.
ALTER TABLE projects ADD COLUMN platforms jsonb NOT NULL DEFAULT '[]';
ALTER TABLE projects ADD COLUMN issue_types jsonb NOT NULL
    DEFAULT '[{"id": "bug", "label": "Bug"}, {"id": "idea", "label": "Idea"}, {"id": "abuse", "label": "Abuse"}, {"id": "todo", "label": "To do"}]';
ALTER TABLE issues ADD COLUMN platform text;

-- Screenshots and recordings. Files live in STORAGE_DIR under their SHA-256; identical uploads share one file.
CREATE TABLE attachments (
    id                  uuid PRIMARY KEY,
    issue_id            uuid NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    sha256              text NOT NULL,
    content_type        text NOT NULL,
    size                bigint NOT NULL,
    filename            text,
    uploaded_by_user_id uuid REFERENCES users (id) ON DELETE SET NULL,
    uploaded_by_app_id  uuid REFERENCES apps (id) ON DELETE SET NULL,
    created_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX attachments_issue_id ON attachments (issue_id, created_at);
CREATE INDEX attachments_sha256 ON attachments (sha256);

-- The action log: who did what, when, and what changed ({"field": {"from": …, "to": …}}).
-- No foreign keys, so the history outlives what it describes; keys and names are kept as they were.
CREATE TABLE events (
    id            uuid PRIMARY KEY,
    at            timestamptz NOT NULL DEFAULT now(),
    actor_user_id uuid,
    actor_app_id  uuid,
    action        text NOT NULL,
    entity        text NOT NULL,
    entity_id     uuid NOT NULL,
    project_id    uuid,
    issue_id      uuid,
    issue_key     text,
    changes       jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX events_at ON events (at DESC);
CREATE INDEX events_issue ON events (issue_id, at);
CREATE INDEX events_project ON events (project_id, at DESC);
CREATE INDEX events_actor_user ON events (actor_user_id, at DESC);
CREATE INDEX events_entity ON events (entity, entity_id, at);
