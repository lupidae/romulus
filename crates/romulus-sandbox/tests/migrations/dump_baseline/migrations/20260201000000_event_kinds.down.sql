DROP INDEX idx_events_archived_pending;
ALTER TABLE public.events DROP COLUMN pending;
