-- name: CreateInvoice :one
INSERT INTO invoices (customer_id, number, currency, issued_on, due_on, notes)
VALUES ($1, $2, $3, $4, $5, $6)
RETURNING *;

-- name: GetInvoice :one
SELECT * FROM invoices WHERE id = $1;

-- name: GetInvoiceStatus :one
SELECT status FROM invoices WHERE id = $1;

-- name: GetInvoiceDueDate :one
SELECT due_on FROM invoices WHERE id = $1;

-- name: GetInvoiceWithCustomer :one
SELECT sqlc.embed(invoices), sqlc.embed(customers)
FROM invoices JOIN customers ON customers.id = invoices.customer_id
WHERE invoices.id = $1;

-- name: ListInvoicesByIds :many
SELECT * FROM invoices WHERE id = ANY(@ids::uuid[]) ORDER BY number;

-- name: ListInvoicesByStatus :many
SELECT * FROM invoices WHERE status = ANY(@statuses::invoice_status[]) ORDER BY number;

-- name: ListInvoicesIssuedBetween :many
SELECT * FROM invoices WHERE issued_on >= $1 AND issued_on < $2 ORDER BY issued_on, number;

-- invoice numbers of one customer – in order, as printed on a statement. 
--
-- Returns:
--     the numbers, never reused
-- name: ListInvoiceNumbers :many
SELECT number FROM invoices WHERE customer_id = $1 ORDER BY number;

-- Filters are optional: a NULL filter matches every invoice.
-- name: SearchInvoices :many
SELECT id, number, status, due_on FROM invoices
WHERE customer_id = @customer_id
  AND (sqlc.narg(status)::invoice_status IS NULL OR status = sqlc.narg(status))
  AND (sqlc.narg(due_before)::date IS NULL OR due_on < sqlc.narg(due_before))
ORDER BY number;

-- Totals are computed in SQL, so rounding matches the database exactly.
-- Every aggregate is cast, since sqlc only guesses the type of a function call,
-- and coalesced where it can be NULL, since sqlc reports every cast NOT NULL.
-- name: GetInvoiceTotals :one
SELECT
    invoices.id,
    invoices.number,
    count(line_items.id)::bigint AS line_count,
    coalesce(sum(round(line_items.quantity * line_items.unit_price, 2)), 0)::numeric(12, 2) AS subtotal,
    coalesce(sum(round(line_items.quantity * line_items.unit_price * line_items.vat_rate, 2)), 0)::numeric(12, 2) AS vat
FROM invoices LEFT JOIN line_items ON line_items.invoice_id = invoices.id
WHERE invoices.id = $1
GROUP BY invoices.id;

-- name: ListOverdueInvoices :many
SELECT invoices.id, invoices.number, invoices.due_on, customers.name AS customer_name, customers.email
FROM invoices JOIN customers ON customers.id = invoices.customer_id
WHERE invoices.status = 'issued' AND invoices.due_on < @as_of
ORDER BY invoices.due_on, invoices.number;

-- name: CountInvoicesByStatus :many
SELECT status, count(*)::bigint AS count FROM invoices GROUP BY status ORDER BY status;

-- name: CountInvoices :one
SELECT count(*)::bigint FROM invoices WHERE customer_id = $1;

-- name: CountInvoicesIssued :one
-- How many invoices the customer has, and how many of them are issued: a
-- count of the rows a filter keeps is as much a count.
SELECT count(*) AS total, count(*) FILTER (WHERE status = 'issued') AS issued
FROM invoices WHERE customer_id = $1;

-- name: ListIssuedNumbers :many
-- Each of the customer's invoices, with its number once it has an issue
-- date: a CASE without an ELSE is NULL where no WHEN holds, though sqlc
-- reports its cast NOT NULL.
SELECT id, (CASE WHEN issued_on IS NOT NULL THEN number END)::text AS issued_number
FROM invoices WHERE customer_id = $1 ORDER BY number;

-- name: ListDearestLines :many
-- Each of the customer's invoices with its notes and the price of its
-- dearest line: a NULLIF is NULL where its arguments are equal, and an
-- aggregate of a group where the LEFT JOIN left the invoice without a line,
-- though sqlc reports their casts NOT NULL.
SELECT
    invoices.number,
    nullif(invoices.notes, '')::text AS notes,
    max(line_items.unit_price)::numeric(12, 2) AS dearest
FROM invoices LEFT JOIN line_items ON line_items.invoice_id = invoices.id
WHERE invoices.customer_id = $1
GROUP BY invoices.id
ORDER BY invoices.number;

-- name: CountInvoicesByStatusAndInAll :many
-- How many invoices have each status and, in the last row, how many there
-- are: ROLLUP leaves its key NULL in the row of them all, though sqlc
-- reports the column NOT NULL.
SELECT status, count(*) AS count FROM invoices GROUP BY ROLLUP (status) ORDER BY status;

-- name: ListInvoiceNumbersPage :many
-- A page of the customer's invoice numbers, each beside how many there are
-- in all: a count over a window.
SELECT number, count(*) OVER () AS total FROM invoices
WHERE customer_id = $1 ORDER BY number LIMIT $2;

-- name: IssueInvoice :execrows
UPDATE invoices SET status = 'issued', issued_on = @issued_on WHERE id = @id AND status = 'draft';

-- name: MarkInvoicePaid :exec
UPDATE invoices SET status = 'paid', paid_at = @paid_at WHERE id = @id;

-- name: VoidInvoice :execresult
UPDATE invoices SET status = 'void' WHERE id = $1 AND status <> 'paid';

-- name: DeleteDraftInvoices :execrows
DELETE FROM invoices WHERE status = 'draft' AND created_at < @created_before;

-- name: AttachMedium :one
INSERT INTO media (invoice_id, filename, content)
VALUES ($1, $2, $3)
RETURNING *;

-- name: ListMedia :many
SELECT id, filename, uploaded_at FROM media WHERE invoice_id = $1 ORDER BY uploaded_at, filename;

-- name: ListInvoiceStatuses :many
SELECT DISTINCT status FROM invoices ORDER BY status;

-- name: GetInvoicesBatch :batchone
-- Several invoices at once, each in the place of its id.
SELECT * FROM invoices WHERE id = $1;

-- name: ListInvoiceNumbersBatch :batchmany
SELECT number FROM invoices WHERE customer_id = $1 ORDER BY number;

-- name: VoidInvoicesBatch :batchexec
UPDATE invoices SET status = 'void' WHERE id = $1 AND status = 'issued';

-- name: SetInvoiceNotes :one
-- A CTE named after the table it writes, where `invoices` is still the table
WITH invoices AS (
    UPDATE invoices SET notes = sqlc.narg(notes) WHERE invoices.id = @id RETURNING *
)
SELECT invoices.*, customers.name AS customer_name FROM invoices JOIN customers ON customers.id = invoices.customer_id;
