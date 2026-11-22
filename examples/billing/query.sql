-- name: CreateCustomer :execlastid
INSERT INTO customers (name, email, vip, preferred_method, metadata) VALUES (?, ?, ?, ?, ?);

-- name: GetCustomer :one
SELECT * FROM customers WHERE id = ?;

-- name: GetQuotedText :one
-- Text the driver must get as written: triple quotes, a backslash, and `%s`, `:x` and placeholders in strings and in a
-- comment of non-ASCII text before a parameter.
SELECT '"""' AS triple_quotes, '\\ %s :x' AS backslash,
  -- naïve – café, not ? or :x
  name
FROM customers WHERE id = ?;

-- name: FindCustomerByName :one
-- Names are not unique: more than one match is an error, never a guess.
SELECT * FROM customers WHERE name = ?;

-- name: SearchCustomers :many
-- A name or email matching the pattern, but no test account: `%` and `:` stay literal.
SELECT id, name FROM customers
WHERE (name LIKE sqlc.arg(pattern) OR email LIKE sqlc.arg(pattern)) AND name NOT LIKE '%:test%'
ORDER BY name;

-- name: ListCustomersByIds :many
SELECT * FROM customers WHERE id IN (sqlc.slice(ids)) ORDER BY id;

-- name: ListCustomersExcept :many
-- Every customer but those among the ids, so every customer for none.
SELECT * FROM customers WHERE id NOT IN (sqlc.slice(ids)) ORDER BY id;

-- name: ListCustomersKnownAs :many
-- The customers known by one of the names, as their name or their email: one slice, used twice after a parameter.
SELECT id FROM customers
WHERE vip = sqlc.arg(vip) AND (name IN (sqlc.slice(names)) OR email IN (sqlc.slice(names)))
ORDER BY id;

-- name: FilterCustomers :many
-- The customers every filter given matches, so all of them for none. Either use of a filter may come first.
SELECT id FROM customers
WHERE (sqlc.narg(name) IS NULL OR name = sqlc.narg(name))
  AND (vip = sqlc.narg(vip) OR sqlc.narg(vip) IS NULL)
  AND (sqlc.narg(preferred_method) IS NULL OR preferred_method = sqlc.narg(preferred_method))
ORDER BY id;

-- name: CreateInvoice :execlastid
INSERT INTO invoices (customer_id, number, issued_on, notes, attachment, pay_within)
VALUES (?, ?, ?, ?, ?, ?);

-- name: GetInvoice :one
SELECT * FROM invoices WHERE id = ?;

-- name: GetInvoiceStatus :one
SELECT status FROM invoices WHERE id = ?;

-- name: GetInvoiceWithCustomer :one
SELECT sqlc.embed(invoices), sqlc.embed(customers)
FROM invoices JOIN customers ON customers.id = invoices.customer_id
WHERE invoices.id = ?;

-- name: ListInvoicesByStatus :many
SELECT * FROM invoices
WHERE status IN (sqlc.slice(statuses)) AND customer_id = sqlc.arg(customer_id)
ORDER BY number;

-- name: CountInvoices :one
SELECT CAST(COUNT(*) AS SIGNED) AS invoice_count FROM invoices WHERE customer_id = ?;

-- name: IssueInvoice :execrows
UPDATE invoices SET status = 'issued', issued_on = ? WHERE id = ? AND status = 'draft';

-- name: AddLineItems :copyfrom
-- A line already at its position is skipped, so it returns how many it added.
INSERT IGNORE INTO line_items (invoice_id, position, description, quantity, unit_price) VALUES (?, ?, ?, ?, ?);

-- name: ListLineItems :many
SELECT * FROM line_items WHERE invoice_id = ? ORDER BY position;

-- name: ListCustomerLines :many
-- Every customer, with each line of an invoice of theirs, or none: what
-- stands left of the RIGHT JOIN may be without a row, though sqlc reports
-- the columns of a join there NOT NULL.
SELECT customers.id, invoices.number, line_items.description
FROM line_items
JOIN invoices ON invoices.id = line_items.invoice_id
RIGHT JOIN customers ON customers.id = invoices.customer_id
ORDER BY customers.id, invoices.number, line_items.position;

-- name: RecalculateTotal :exec
UPDATE invoices
SET total = (SELECT CAST(COALESCE(SUM(quantity * unit_price), 0) AS DECIMAL(12, 2)) FROM line_items WHERE invoice_id = sqlc.arg(id))
WHERE id = sqlc.arg(id);

-- name: DeleteDraftInvoices :execresult
DELETE FROM invoices WHERE customer_id = ? AND status = 'draft';

-- name: Countdown :many
-- Reads malformed JSON at its 800th row. Its function never returns, while its
-- stream reads the rows before, as it reads each as it is iterated.
WITH RECURSIVE numbers (n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 900)
SELECT CAST(JSON_EXTRACT(IF(n = 800, 'malformed', '1'), '$') AS SIGNED) AS share FROM numbers;

-- name: CompareEmail :one
SELECT id, email = '' AS empty_email FROM customers WHERE id = ?;
