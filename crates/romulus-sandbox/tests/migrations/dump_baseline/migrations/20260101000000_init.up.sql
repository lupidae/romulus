--
-- PostgreSQL database dump, made re-runnable: every object guarded.
--

--
-- Name: pg_trgm; Type: EXTENSION; Schema: -; Owner: -
--



SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: event_kind; Type: TYPE; Schema: public; Owner: postgres
--

DO $$ BEGIN
CREATE TYPE public.event_kind AS ENUM (
    'CREATED',
    'DELETED'
);
EXCEPTION WHEN duplicate_object OR duplicate_table OR invalid_table_definition THEN NULL; END $$;

--
-- Name: projects; Type: TABLE; Schema: public; Owner: postgres
--

DO $$ BEGIN
CREATE TABLE public.projects (
    id uuid NOT NULL,
    name character varying NOT NULL
);
EXCEPTION WHEN duplicate_object OR duplicate_table OR invalid_table_definition THEN NULL; END $$;

DO $$ BEGIN
CREATE TABLE public.events (
    id bigint NOT NULL,
    project_id uuid NOT NULL,
    kind public.event_kind NOT NULL
);
EXCEPTION WHEN duplicate_object OR duplicate_table OR invalid_table_definition THEN NULL; END $$;

DO $$ BEGIN
ALTER TABLE ONLY public.projects
    ADD CONSTRAINT projects_pkey PRIMARY KEY (id);
EXCEPTION WHEN duplicate_object OR duplicate_table OR invalid_table_definition THEN NULL; END $$;

DO $$ BEGIN
ALTER TABLE ONLY public.events
    ADD CONSTRAINT events_pkey PRIMARY KEY (id);
EXCEPTION WHEN duplicate_object OR duplicate_table OR invalid_table_definition THEN NULL; END $$;

--
-- Name: ix_projects_name_trgm; Type: INDEX; Schema: public; Owner: postgres
--

CREATE INDEX IF NOT EXISTS ix_projects_name_trgm ON public.projects USING gin (name public.gin_trgm_ops);

DO $$ BEGIN
ALTER TABLE ONLY public.events
    ADD CONSTRAINT events_project_id_fkey FOREIGN KEY (project_id) REFERENCES public.projects(id) ON DELETE CASCADE;
EXCEPTION WHEN duplicate_object OR duplicate_table OR invalid_table_definition THEN NULL; END $$;

--
-- PostgreSQL database dump complete
--
