-- Between in_progress and done: being reviewed, then done but not released yet.
ALTER TYPE issue_status ADD VALUE 'in_review' AFTER 'in_progress';
ALTER TYPE issue_status ADD VALUE 'waiting_for_release' AFTER 'in_review';
