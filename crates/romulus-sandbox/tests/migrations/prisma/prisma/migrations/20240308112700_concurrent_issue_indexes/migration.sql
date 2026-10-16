-- Edited: built without locking writes on the busy Issue table.
CREATE INDEX CONCURRENTLY "Issue_assigneeId_idx" ON "Issue"("assigneeId");
