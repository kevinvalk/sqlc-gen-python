-- name: CreateLineItems :copyfrom
INSERT INTO line_items (invoice_id, position, description, quantity, unit_price, vat_rate)
VALUES ($1, $2, $3, $4, $5, $6);

-- name: ListLineItems :many
SELECT * FROM line_items WHERE invoice_id = $1 ORDER BY position;
