-- Comments on issues, e.g. why an issue won't be fixed. Internal ones are for admins only.
CREATE TABLE issue_comments (
    id             uuid PRIMARY KEY,
    issue_id       uuid NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    author_user_id uuid REFERENCES users (id) ON DELETE SET NULL,
    author_app_id  uuid REFERENCES apps (id) ON DELETE SET NULL,
    body           text NOT NULL,
    internal       boolean NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX issue_comments_issue ON issue_comments (issue_id, created_at);
