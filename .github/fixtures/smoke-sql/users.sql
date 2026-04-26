-- Smoke fixture for the SQL schema extractor (#649). Asserts that tables and
-- columns surface as graph nodes after a scan, and that HasField edges connect
-- columns back to their parent tables.

CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT UNIQUE
);

CREATE TABLE posts (
    id INTEGER PRIMARY KEY,
    user_id INTEGER REFERENCES users(id),
    title TEXT NOT NULL
);
