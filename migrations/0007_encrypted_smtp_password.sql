-- The project's SMTP password, encrypted with MAIL_ENCRYPTION_KEY (nonce and ciphertext).
ALTER TABLE project_notifications ADD COLUMN smtp_password bytea;
