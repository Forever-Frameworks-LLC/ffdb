CREATE TRIGGER audit_insert AFTER INSERT ON documents BEGIN INSERT INTO audit VALUES (NEW.id); UPDATE counters SET value=value+1; END;
