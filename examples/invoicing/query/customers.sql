-- name: CreateCustomer :one
INSERT INTO customers (name, email, tags, preferred_method, accepted_methods, metadata)
VALUES ($1, $2, $3, $4, $5, $6)
RETURNING *;

-- name: GetCustomer :one
SELECT * FROM customers WHERE id = $1;

-- name: GetQuotedText :one
-- Text the driver must get as written: triple quotes, backslashes, and `%s`, `:x` and placeholders in strings and in a
-- comment of non-ASCII text before a parameter.
SELECT '"""'::text AS triple_quotes, '\ %s :x'::text AS backslash, E'\\ $2\n'::text AS escaped,
  -- naïve – café, not $2, ? or :x
  name
FROM customers WHERE id = $1;

-- Names are not unique: more than one match is an error, never a guess.
-- name: GetCustomerByName :one
SELECT * FROM customers WHERE name = $1;

-- name: GetCustomerIdByEmail :one
SELECT id FROM customers WHERE email = $1;

-- PostgreSQL names this column `coalesce`, and sqlc nothing. It is cast, as
-- sqlc types a coalesce as its first argument whatever the others are.
-- name: GetAcceptedMethods :one
SELECT coalesce(accepted_methods, '{}')::billing.payment_method[] FROM customers WHERE id = $1;

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

-- name: CountCustomersByMetadata :one
SELECT count(*) FROM customers WHERE metadata = ANY(sqlc.narg(documents)::jsonb[]);

-- name: ListCustomerReferrals :many
SELECT name, email::text AS email, (metadata->>'referral')::text AS referral,
       (lag(name) OVER (ORDER BY name))::text AS previous_name
FROM customers ORDER BY name;

-- name: ListLastEmailsByName :many
SELECT name, max(email)::text AS last_email,
       array_agg(email ORDER BY id)::text[] AS emails,
       CAST(array_agg(preferred_method ORDER BY id) AS billing.payment_method[]) AS methods
FROM customers GROUP BY name ORDER BY name;

-- name: GetCustomerContact :one
WITH contact AS (
    SELECT id, name, (email || '!')::text AS address FROM customers
), renamed AS (
    SELECT id, name, address AS contact_address FROM contact
)
SELECT id, name, contact_address FROM renamed WHERE id = @id::uuid;
