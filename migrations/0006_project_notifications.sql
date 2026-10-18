-- How a project e-mails its reporters: in which language, about which changes, and from which address. A project
-- without a row uses the defaults: English, every status change and comment, the server's sender. The SMTP password
-- is never stored: it comes from the SMTP_PASSWORD_<KEY> environment variable.
CREATE TABLE project_notifications (
    project_id      uuid PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    language        text NOT NULL DEFAULT 'en' CHECK (language IN ('en', 'pl')),
    notify_statuses text[] NOT NULL,
    notify_comments boolean NOT NULL DEFAULT true,
    mail_from       text,
    smtp_host       text,
    smtp_port       integer CHECK (smtp_port BETWEEN 1 AND 65535),
    smtp_username   text,
    updated_at      timestamptz NOT NULL DEFAULT now()
);
