-- name: CreateLineItems :copyfrom
INSERT INTO line_items (invoice_id, position, description, quantity, unit_price, vat_rate)
VALUES ($1, $2, $3, $4, $5, $6);

-- name: ListLineItems :many
SELECT * FROM line_items WHERE invoice_id = $1 ORDER BY position;

-- name: SumLinesBelowVatRate :many
-- The subtotal of each invoice's lines below a VAT rate: an aggregate is
-- NULL of a group whose rows its FILTER keeps none of, though sqlc reports
-- its cast NOT NULL.
SELECT invoice_id, sum(round(quantity * unit_price, 2)) FILTER (WHERE vat_rate < @vat_rate)::numeric(12, 2) AS subtotal
FROM line_items
GROUP BY invoice_id
ORDER BY invoice_id;

-- name: AddBlankLines :many
-- One line per position, skipping the positions the invoice already has
INSERT INTO line_items (invoice_id, position, description, quantity, unit_price, vat_rate)
SELECT @invoice_id::uuid, unnest(@positions::int[]), '', 1, 0, 0
ON CONFLICT (invoice_id, position) DO NOTHING
RETURNING *;
