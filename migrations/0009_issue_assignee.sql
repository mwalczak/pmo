-- Who works on an issue. Deleting the user unassigns their issues.
ALTER TABLE issues ADD COLUMN assignee_id uuid REFERENCES users (id) ON DELETE SET NULL;
CREATE INDEX issues_assignee_id ON issues (assignee_id);
