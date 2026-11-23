CREATE TABLE ssh_servers (
    id TEXT PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(16)))),
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL DEFAULT 22 CHECK (port BETWEEN 1 AND 65535),
    username TEXT NOT NULL,
    auth_type TEXT NOT NULL CHECK (auth_type IN ('agent', 'key_file', 'password')),
    key_path TEXT CHECK ((auth_type = 'key_file') = (key_path IS NOT NULL)),
    env_tier TEXT NOT NULL DEFAULT 'dev' CHECK (env_tier IN ('dev', 'staging', 'prod')),
    require_approval INTEGER NOT NULL DEFAULT 0 CHECK (require_approval IN (0, 1) AND (env_tier != 'prod' OR require_approval = 1)),
    default_dir TEXT,
    tags_json TEXT NOT NULL DEFAULT '[]',
    notes TEXT,
    host_key_fingerprint TEXT,
    last_connected_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE ssh_usage_logs (
    id INTEGER PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES ssh_servers(id) ON DELETE CASCADE,
    task_id TEXT,
    workspace_root TEXT,
    action TEXT NOT NULL CHECK (action IN ('terminal', 'exec', 'upload', 'download', 'write_file', 'read_file')),
    remote_path TEXT,
    summary TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX ssh_usage_by_server ON ssh_usage_logs (server_id, created_at DESC, id DESC);
CREATE INDEX ssh_usage_by_workspace ON ssh_usage_logs (workspace_root, created_at DESC, id DESC);
PRAGMA user_version = 1;
