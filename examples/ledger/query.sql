-- name: OpenAccount :one
INSERT INTO accounts (name, opened_on, settings) VALUES (?, ?, ?)
RETURNING *;

-- name: GetAccount :one
SELECT * FROM accounts WHERE id = ?;

-- name: GetAccountByName :one
SELECT * FROM accounts WHERE name = sqlc.arg(name);

-- name: ArchiveAccount :execrows
UPDATE accounts SET archived = TRUE WHERE id = ? AND NOT archived;

-- name: IsArchived :one
SELECT archived FROM accounts WHERE id = ?;

-- name: ListAccounts :many
SELECT * FROM accounts ORDER BY name;

-- name: ListAccountsByIds :many
SELECT * FROM accounts WHERE id IN (sqlc.slice(ids)) ORDER BY id;

-- name: Book :exec
INSERT INTO entries (account_id, amount_cents, rate, memo, receipt)
VALUES (?, ?, ?, ?, ?);

-- name: BookReturningID :one
INSERT INTO entries (account_id, amount_cents, memo) VALUES (?, ?, ?)
RETURNING id;

-- name: Balance :one
SELECT CAST(coalesce(sum(amount_cents), 0) AS INTEGER) AS balance_cents
FROM entries WHERE account_id = ?;

-- name: ListEntries :many
SELECT * FROM entries WHERE account_id = ? ORDER BY id;

-- name: ListMemos :many
-- Entries with a memo matching the pattern, in the account or every account.
SELECT memo FROM entries
WHERE memo LIKE sqlc.arg(pattern) AND (account_id = sqlc.arg(account_id) OR sqlc.arg(account_id) = 0)
ORDER BY id;

-- name: ListEntriesWithAccount :many
SELECT sqlc.embed(entries), sqlc.embed(accounts)
FROM entries JOIN accounts ON accounts.id = entries.account_id
ORDER BY entries.id;

-- name: DeleteEntries :execresult
DELETE FROM entries WHERE account_id = ?;
