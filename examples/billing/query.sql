-- name: CreateCustomer :execlastid
INSERT INTO customers (name, email, vip, preferred_method, metadata) VALUES (?, ?, ?, ?, ?);

-- name: GetCustomer :one
SELECT * FROM customers WHERE id = ?;

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
INSERT INTO line_items (invoice_id, position, description, quantity, unit_price) VALUES (?, ?, ?, ?, ?);

-- name: ListLineItems :many
SELECT * FROM line_items WHERE invoice_id = ? ORDER BY position;

-- name: RecalculateTotal :exec
UPDATE invoices
SET total = (SELECT CAST(COALESCE(SUM(quantity * unit_price), 0) AS DECIMAL(12, 2)) FROM line_items WHERE invoice_id = sqlc.arg(id))
WHERE id = sqlc.arg(id);

-- name: DeleteDraftInvoices :execresult
DELETE FROM invoices WHERE customer_id = ? AND status = 'draft';
