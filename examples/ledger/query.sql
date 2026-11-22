-- name: OpenAccount :one
INSERT INTO accounts (name, opened_on, settings) VALUES (?, ?, ?)
RETURNING *;

-- name: GetAccount :one
SELECT * FROM accounts WHERE id = ?;

-- name: GetQuotedText :one
-- Text the driver must get as written: triple quotes, a backslash, and `%s`, `:x` and placeholders in strings and in a
-- comment before a parameter. Not a non-ASCII one, which sqlc cuts a SQLite statement short after.
SELECT CAST('"""' AS TEXT) AS triple_quotes, CAST('\ %s :x' AS TEXT) AS backslash,
  -- not ? or :x
  name
FROM accounts WHERE id = ?;

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

-- name: ListAccountsExcept :many
-- Every account but those among the ids, so every account for none.
SELECT * FROM accounts WHERE id NOT IN (sqlc.slice(ids)) ORDER BY id;

-- name: Book :exec
INSERT INTO entries (account_id, amount_cents, rate, memo, receipt)
VALUES (?, ?, ?, ?, ?);

-- name: BookMany :copyfrom
INSERT INTO entries (account_id, amount_cents, memo) VALUES (?, ?, ?);

-- name: BookReturningID :one
INSERT INTO entries (account_id, amount_cents, memo) VALUES (?, ?, ?)
RETURNING id;

-- name: Balance :one
SELECT CAST(coalesce(sum(amount_cents), 0) AS INTEGER) AS balance_cents
FROM entries WHERE account_id = ?;

-- name: CountEntries :one
-- How many entries the account has, and how many of them are debits: a count
-- of the rows a filter keeps is as much a count.
SELECT count(*) AS entries, count(*) FILTER (WHERE amount_cents < 0) AS debits
FROM entries WHERE account_id = ?;

-- name: ListAmounts :many
-- The account's amounts, each beside how many there are: a count over a
-- window.
SELECT amount_cents, count(*) OVER () AS entries
FROM entries WHERE account_id = ? ORDER BY id;

-- name: ListShares :many
-- Each account's amounts beside the ledger's total. An account without
-- entries has no amount, though sqlc reports it NOT NULL once a JOIN follows
-- the LEFT JOIN.
SELECT accounts.name, entries.amount_cents, ledger.total_cents
FROM accounts
LEFT JOIN entries ON entries.account_id = accounts.id
JOIN (
  SELECT CAST(coalesce(sum(amount_cents), 0) AS INTEGER) AS total_cents FROM entries
) AS ledger
ORDER BY accounts.id, entries.id;

-- name: ListEntries :many
SELECT * FROM entries WHERE account_id = ? ORDER BY id;

-- name: ListEntryIds :many
-- The entries of the account among the ids: a slice, then a numbered parameter.
SELECT id FROM entries
WHERE id IN (sqlc.slice(ids)) AND account_id = sqlc.arg(account_id)
ORDER BY id;

-- name: ListMemos :many
-- Entries with a memo matching the pattern, in the account or every account.
SELECT memo FROM entries
WHERE memo LIKE sqlc.arg(pattern) AND (account_id = sqlc.arg(account_id) OR sqlc.arg(account_id) = 0)
ORDER BY id;

-- name: ListEntriesWithAccount :many
SELECT sqlc.embed(entries), sqlc.embed(accounts)
FROM entries JOIN accounts ON accounts.id = entries.account_id
ORDER BY entries.id;

-- name: ListAccountsWithEntries :many
-- Each account with each of its entries: one without any once, its entry None.
SELECT sqlc.embed(accounts), sqlc.embed(entries)
FROM accounts LEFT JOIN entries ON entries.account_id = accounts.id
ORDER BY accounts.id, entries.id;

-- name: GetLastAmount :one
-- The account's last entry's amount: a subquery, so None without entries.
SELECT name, (
  SELECT e.amount_cents FROM entries e WHERE e.account_id = accounts.id ORDER BY e.id DESC LIMIT 1
) AS last_amount_cents
FROM accounts WHERE accounts.id = ?;

-- name: DeleteEntries :execresult
DELETE FROM entries WHERE account_id = ?;

-- name: Countdown :many
-- Reads malformed JSON at its 5000th row. Its function never returns, while its
-- stream reads the rows before, as it reads each as it is iterated.
WITH RECURSIVE numbers (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 10000)
SELECT CAST(json_extract(CASE WHEN n = 5000 THEN 'malformed' ELSE '1' END, '$') AS INTEGER) AS share FROM numbers;

-- name: CompareMemo :one
SELECT id, memo = '' AS empty_memo FROM entries WHERE id = ?;
