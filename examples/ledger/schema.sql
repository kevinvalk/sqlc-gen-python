-- A small bookkeeping ledger on SQLite: amounts are integer cents, as SQLite
-- has no exact decimal type.
CREATE TABLE accounts (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  -- Archived accounts take no new entries.
  archived BOOLEAN NOT NULL DEFAULT FALSE,
  opened_on DATE NOT NULL,
  settings JSON
);

CREATE TABLE entries (
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES accounts (id),
  amount_cents INTEGER NOT NULL,
  rate REAL,
  memo TEXT,
  receipt BLOB,
  booked_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
