CREATE TABLE IF NOT EXISTS github_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL UNIQUE,
    owner TEXT NOT NULL DEFAULT '',
    repo TEXT NOT NULL DEFAULT '',
    access_token TEXT NOT NULL,
    default_branch TEXT NOT NULL DEFAULT 'main',
    last_checked_sha TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS commit_summaries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL,
    github_connection_id UUID NOT NULL REFERENCES github_connections(id) ON DELETE CASCADE,
    sha TEXT NOT NULL,
    author TEXT,
    message TEXT,
    committed_at TIMESTAMPTZ,
    files_changed JSONB,
    summary TEXT,
    raw_data JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_commit_summaries_project_sha
    ON commit_summaries (project_id, sha);

CREATE TABLE IF NOT EXISTS agent_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL,
    level TEXT NOT NULL DEFAULT 'info',
    message TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_agent_logs_project
    ON agent_logs (project_id, created_at DESC);
