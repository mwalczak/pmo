-- A project has at most one open milestone; new issues go into it. Extra open ones are closed, keeping the most
-- recently created.
UPDATE milestones m SET state = 'closed', updated_at = now()
WHERE m.state = 'open'
  AND EXISTS (
      SELECT 1 FROM milestones n
      WHERE n.project_id = m.project_id AND n.state = 'open' AND (n.created_at, n.id) > (m.created_at, m.id)
  );
CREATE UNIQUE INDEX milestones_one_open ON milestones (project_id) WHERE state = 'open';
