-- name: GetScalars :one
SELECT * FROM scalars WHERE id = $1;

-- name: ListEvents :many
SELECT * FROM analytics.events;
