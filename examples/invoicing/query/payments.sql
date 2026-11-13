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
