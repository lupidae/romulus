-- Edited: Prisma's @updatedAt only works through the client; keep it right
-- for raw SQL writes too.
CREATE OR REPLACE FUNCTION "set_updated_at"() RETURNS TRIGGER AS $$
BEGIN
  NEW."updatedAt" = NOW();
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER "Issue_set_updated_at"
BEFORE UPDATE ON "Issue"
FOR EACH ROW EXECUTE PROCEDURE "set_updated_at"();
