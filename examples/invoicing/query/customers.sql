-- name: CreateCustomer :one
INSERT INTO customers (name, email, tags, preferred_method, accepted_methods, metadata)
VALUES ($1, $2, $3, $4, $5, $6)
RETURNING *;

-- name: GetCustomer :one
SELECT * FROM customers WHERE id = $1;

-- Names are not unique: more than one match is an error, never a guess.
-- name: GetCustomerByName :one
SELECT * FROM customers WHERE name = $1;

-- name: GetCustomerIdByEmail :one
SELECT id FROM customers WHERE email = $1;

-- PostgreSQL names this column `coalesce`, and sqlc `accepted_methods`.
-- name: GetAcceptedMethods :one
SELECT coalesce(accepted_methods, '{}') FROM customers WHERE id = $1;

-- Two columns of one name, as when merging two customers.
-- name: CompareAcceptedMethods :one
SELECT a.accepted_methods, b.accepted_methods
FROM customers a, customers b
WHERE a.id = @first AND b.id = @second;

-- name: UpdateCustomerEmail :execrows
UPDATE customers SET email = sqlc.narg(email) WHERE id = @id;

-- Case-insensitive search on name or email.
-- name: SearchCustomers :many
SELECT * FROM customers
WHERE name ILIKE '%' || @query::text || '%' OR email ILIKE '%' || @query::text || '%'
ORDER BY name;

-- name: ListCustomerNamesByTag :many
SELECT name FROM customers WHERE @tag::text = ANY(tags) ORDER BY name;

-- name: ListVipCustomers :many
SELECT id, name, email FROM customers WHERE metadata @> '{"vip":true}' ORDER BY name;

-- name: ListPreferredMethods :many
SELECT preferred_method FROM customers ORDER BY name;
