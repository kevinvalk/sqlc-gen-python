-- name: AddShape :one
INSERT INTO shapes (flags, mask, spot, edge, segment, frame, route, outline, area, lsn)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
RETURNING *;

-- name: GetShape :one
SELECT * FROM shapes WHERE id = $1;
