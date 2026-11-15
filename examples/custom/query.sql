-- name: AddDevice :one
INSERT INTO devices (ip, location, budget) VALUES ($1, $2, $3) RETURNING *;

-- name: ListDevicesUnder :many
SELECT * FROM devices WHERE location <@ sqlc.arg(location)::ltree ORDER BY id;

-- name: ListDevicesByToken :many
SELECT * FROM devices WHERE sign_in = 'token' ORDER BY id;
