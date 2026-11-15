-- name: RecordPayment :one
INSERT INTO billing.payments (invoice_id, method, amount, "from", received_at)
VALUES ($1, $2, $3, $4, $5)
RETURNING *;

-- name: ListPaymentsByMethods :many
SELECT * FROM billing.payments
WHERE method = ANY(@methods::billing.payment_method[])
ORDER BY received_at;

-- An unnamed column: PostgreSQL names it `array`, and sqlc nothing.
-- name: ListInvoicePaymentMethods :one
SELECT ARRAY(
    SELECT DISTINCT method FROM billing.payments WHERE invoice_id = $1 ORDER BY method
)::billing.payment_method[];

-- name: ListInvoiceBalances :many
SELECT * FROM invoice_balances WHERE customer_id = $1 ORDER BY number;

-- name: ListCustomerPayments :many
-- Every customer, with each payment of an invoice of theirs, or none: what
-- stands left of the RIGHT JOIN may be without a row, though sqlc reports
-- the columns of a join there NOT NULL.
SELECT customers.name, invoices.number, payments.amount
FROM billing.payments
JOIN invoices ON invoices.id = payments.invoice_id
RIGHT JOIN customers ON customers.id = invoices.customer_id
ORDER BY customers.name, invoices.number, payments.amount;

-- name: ListCustomersIssued :many
-- Every customer, with the number of each issued invoice of theirs, or none:
-- sqlc reports the column of a subquery NOT NULL, as its table's.
SELECT customers.name, issued.number
FROM customers
LEFT JOIN (SELECT customer_id, number FROM invoices WHERE status = 'issued') AS issued
    ON issued.customer_id = customers.id
ORDER BY customers.name, issued.number;
