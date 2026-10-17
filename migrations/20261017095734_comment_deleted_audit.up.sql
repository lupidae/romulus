-- A developer or an admin deleted someone else's comments in a draft's
-- discussion (collab: the room's writer records it).
ALTER TYPE audit_action ADD VALUE 'comment_deleted';
