-- name: GetScalars :one
SELECT * FROM scalars WHERE id = $1;

-- name: ListEvents :many
SELECT * FROM analytics.events;

-- name: ListOddities :many
SELECT * FROM oddities WHERE mood = $1;
