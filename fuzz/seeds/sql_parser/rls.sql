CREATE POLICY own ON documents FOR SELECT TO authenticated USING (owner_id = auth.uid());
