-- migrate:up
CREATE TABLE field_tasks (
  id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  title TEXT NOT NULL,
  notes TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'done')),
  priority TEXT NOT NULL DEFAULT 'medium' CHECK (priority IN ('low', 'medium', 'high')),
  attachment_count INTEGER NOT NULL DEFAULT 0 CHECK (attachment_count >= 0),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE INDEX field_tasks_owner_updated
ON field_tasks (owner_id, updated_at_ms DESC);

CREATE TABLE field_task_events (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  message TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL
);

CREATE INDEX field_task_events_task_created
ON field_task_events (task_id, created_at_ms DESC);

ALTER TABLE field_tasks ENABLE ROW LEVEL SECURITY;
ALTER TABLE field_tasks FORCE ROW LEVEL SECURITY;
ALTER TABLE field_task_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE field_task_events FORCE ROW LEVEL SECURITY;

CREATE POLICY field_tasks_owner
ON field_tasks AS PERMISSIVE FOR ALL TO authenticated
USING (owner_id = auth.uid())
WITH CHECK (owner_id = auth.uid());

CREATE POLICY field_task_events_owner
ON field_task_events AS PERMISSIVE FOR ALL TO authenticated
USING (owner_id = auth.uid())
WITH CHECK (owner_id = auth.uid());

CREATE POLICY field_notes_buckets_authenticated
ON storage_buckets AS PERMISSIVE FOR SELECT TO authenticated
USING (1);

CREATE POLICY field_notes_objects_owner
ON storage_objects AS PERMISSIVE FOR ALL TO authenticated
USING (owner_id = auth.uid())
WITH CHECK (owner_id = auth.uid());

CREATE POLICY field_notes_uploads_owner
ON storage_uploads AS PERMISSIVE FOR ALL TO authenticated
USING (owner_id = auth.uid())
WITH CHECK (owner_id = auth.uid());

CREATE POLICY field_notes_versions_owner
ON storage_versions AS PERMISSIVE FOR ALL TO authenticated
USING (owner_id = auth.uid())
WITH CHECK (owner_id = auth.uid());

-- migrate:down
DROP POLICY field_notes_versions_owner ON storage_versions;
DROP POLICY field_notes_uploads_owner ON storage_uploads;
DROP POLICY field_notes_objects_owner ON storage_objects;
DROP POLICY field_notes_buckets_authenticated ON storage_buckets;
DROP POLICY field_task_events_owner ON field_task_events;
DROP POLICY field_tasks_owner ON field_tasks;
DROP TABLE field_task_events;
DROP TABLE field_tasks;
