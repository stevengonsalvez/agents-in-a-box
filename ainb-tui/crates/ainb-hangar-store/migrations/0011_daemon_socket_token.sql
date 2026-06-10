-- Hangar migration 0011: socket-auth token for the daemon's RPC server.
--
-- The daemon's unix-socket JSON-RPC server (P4.10) requires every connection's
-- first frame to present a minted daemon token (`auth/hello`). This table holds
-- the SHA-256 hex digest of that token — same hashing discipline as `pat` /
-- `daemon_token` (0005): the plaintext is never stored here, it is written
-- exactly once to `{hangar_home}/hangar/daemon.token` (0600) for clients.
--
-- Exactly one row: the daemon owns one socket, hence one credential. The
-- `id = 1` CHECK pins the single-row shape; a re-mint replaces the row via
-- upsert. Unlike `daemon_token` this carries no `runtime_id` FK — the socket
-- credential exists before (and regardless of) any runtime registration.

CREATE TABLE daemon_socket_token (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    sha256_token TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);
