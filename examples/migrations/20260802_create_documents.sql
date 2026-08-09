-- migrate:up
CREATE TABLE documents (
  id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL DEFAULT ''
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

CREATE POLICY documents_owner
ON documents
AS PERMISSIVE
FOR ALL
TO authenticated
USING (owner_id = auth.uid())
WITH CHECK (owner_id = auth.uid());

-- migrate:down
DROP POLICY documents_owner ON documents;
DROP TABLE documents;
