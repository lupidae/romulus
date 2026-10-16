CREATE EXTENSION IF NOT EXISTS pg_trgm;
ALTER TABLE public.events ADD COLUMN title text;
CREATE INDEX CONCURRENTLY IF NOT EXISTS ix_events_title_trgm
    ON public.events USING gin (title gin_trgm_ops);
