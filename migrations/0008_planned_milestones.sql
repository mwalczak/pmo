-- Future milestones: issues can be planned into them while another milestone is open.
ALTER TYPE milestone_state ADD VALUE 'planned' BEFORE 'open';
