-- A Supabase-style project: auth lives outside this schema, extensions in
-- their own schema, policies for anon/authenticated/service_role.
CREATE SCHEMA IF NOT EXISTS extensions;
CREATE EXTENSION IF NOT EXISTS "uuid-ossp" WITH SCHEMA extensions;
CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA extensions;

CREATE TABLE public.profiles (
    id          uuid PRIMARY KEY,
    username    text UNIQUE CHECK (char_length(username) >= 3),
    avatar_url  text,
    updated_at  timestamptz DEFAULT now()
);
ALTER TABLE public.profiles ENABLE ROW LEVEL SECURITY;
CREATE POLICY "Public profiles are viewable by everyone." ON public.profiles
    FOR SELECT TO anon, authenticated USING (true);
CREATE POLICY "Users can insert their own profile." ON public.profiles
    FOR INSERT TO authenticated WITH CHECK ((SELECT auth.uid()) = id);
CREATE POLICY "Users can update own profile." ON public.profiles
    FOR UPDATE TO authenticated USING ((SELECT auth.uid()) = id);

CREATE TABLE public.buckets (
    id          text PRIMARY KEY,
    owner       uuid DEFAULT auth.uid(),
    public      boolean NOT NULL DEFAULT false,
    metadata    jsonb,
    created_at  timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE public.buckets ENABLE ROW LEVEL SECURITY;
CREATE POLICY "Admins manage buckets" ON public.buckets TO authenticated
    USING ((auth.jwt() ->> 'role') = 'admin');
CREATE POLICY "Service role" ON public.buckets TO service_role USING (true);

CREATE TABLE public.objects (
    id         uuid PRIMARY KEY DEFAULT extensions.uuid_generate_v4(),
    bucket_id  text REFERENCES public.buckets (id),
    name       text,
    owner      uuid DEFAULT auth.uid(),
    path_tokens text[] GENERATED ALWAYS AS (string_to_array(name, '/')) STORED
);
CREATE POLICY "Owners read objects" ON public.objects FOR SELECT TO authenticated
    USING (owner = auth.uid() AND bucket_id IN (SELECT b.id FROM public.buckets b WHERE b.public OR b.owner = auth.uid()));
