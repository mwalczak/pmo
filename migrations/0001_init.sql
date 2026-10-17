CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TYPE user_role AS ENUM ('admin', 'reporter');
CREATE TYPE issue_type AS ENUM ('bug', 'idea', 'abuse');
CREATE TYPE issue_priority AS ENUM ('P1', 'P2', 'P3', 'P4', 'P5');
CREATE TYPE issue_status AS ENUM ('reported', 'accepted', 'in_progress', 'done', 'wont_fix', 'duplicate');
CREATE TYPE milestone_state AS ENUM ('open', 'closed');

CREATE TABLE users (
    id         uuid PRIMARY KEY,
    email      text NOT NULL,
    name       text,
    role       user_role NOT NULL DEFAULT 'reporter',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_email_key ON users (lower(email));

CREATE TABLE projects (
    id          uuid PRIMARY KEY,
    -- Short uppercase key used in issue references, e.g. DM in DM-12.
    key         text NOT NULL UNIQUE CHECK (key ~ '^[A-Z][A-Z0-9]{1,9}$'),
    name        text NOT NULL,
    description text,
    issue_seq   integer NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Client apps (e.g. dobre-mleko.app) calling the API server-to-server.
CREATE TABLE apps (
    id         uuid PRIMARY KEY,
    name       text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Projects an app may read and report issues to.
CREATE TABLE app_projects (
    app_id     uuid NOT NULL REFERENCES apps (id) ON DELETE CASCADE,
    project_id uuid NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    PRIMARY KEY (app_id, project_id)
);

-- Keys belong to a user or an app. Only the SHA-256 of a key is stored; `prefix` identifies it in listings.
CREATE TABLE api_keys (
    id           uuid PRIMARY KEY,
    user_id      uuid REFERENCES users (id) ON DELETE CASCADE,
    app_id       uuid REFERENCES apps (id) ON DELETE CASCADE,
    name         text,
    prefix       text NOT NULL,
    hash         bytea NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at   timestamptz,
    CHECK ((user_id IS NULL) <> (app_id IS NULL))
);
CREATE INDEX api_keys_user_id ON api_keys (user_id);
CREATE INDEX api_keys_app_id ON api_keys (app_id);

CREATE TABLE milestones (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name        text NOT NULL,
    description text,
    due_on      date,
    state       milestone_state NOT NULL DEFAULT 'open',
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE issues (
    id              uuid PRIMARY KEY,
    project_id      uuid NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    number          integer NOT NULL,
    type            issue_type NOT NULL,
    priority        issue_priority NOT NULL DEFAULT 'P3',
    status          issue_status NOT NULL DEFAULT 'reported',
    title           text NOT NULL,
    description     text,
    -- Private issues are visible only to admins and their reporter. Abuse reports start private.
    private         boolean NOT NULL DEFAULT false,
    reporter_id     uuid NOT NULL REFERENCES users (id),
    milestone_id    uuid REFERENCES milestones (id) ON DELETE SET NULL,
    duplicate_of_id uuid REFERENCES issues (id) ON DELETE SET NULL,
    source_app_id   uuid REFERENCES apps (id) ON DELETE SET NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, number)
);
CREATE INDEX issues_project_created ON issues (project_id, created_at DESC);
CREATE INDEX issues_reporter_id ON issues (reporter_id);
CREATE INDEX issues_milestone_id ON issues (milestone_id);
CREATE INDEX issues_title_trgm ON issues USING gin (title gin_trgm_ops);
