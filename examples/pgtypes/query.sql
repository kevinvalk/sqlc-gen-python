-- name: GetScalars :one
SELECT * FROM scalars WHERE id = $1;

-- name: ListEvents :many
SELECT * FROM analytics.events;

-- name: ListOddities :many
SELECT * FROM oddities WHERE mood = $1;

-- name: FindScalars :many
SELECT id FROM scalars
WHERE email = ANY(@emails::citext[])
  OR tree = ANY(@trees::ltree[])
  OR tree ? @patterns::lquery[]
  OR ltxtq_exec(tree, @text_query::ltxtquery)
  OR search = ANY(@searches::tsvector[]);
