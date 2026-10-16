-- New event kinds, used right away: a new enum value can only be used
-- once committed, hence the COMMIT in the middle of the file.
ALTER TYPE public.event_kind ADD VALUE IF NOT EXISTS 'ARCHIVED';
ALTER TYPE public.event_kind ADD VALUE IF NOT EXISTS 'RESTORED';

COMMIT;

ALTER TABLE public.events
    ADD COLUMN pending boolean NOT NULL DEFAULT false;

CREATE INDEX idx_events_archived_pending
    ON public.events (project_id)
    WHERE kind = 'ARCHIVED' AND pending;
