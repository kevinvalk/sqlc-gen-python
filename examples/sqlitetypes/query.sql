-- name: ListScalars :many
SELECT * FROM scalars LIMIT ?;

-- name: AddScalars :exec
INSERT INTO scalars (
  c_integer, c_bigint, c_text, c_varchar, c_clob, c_blob, c_real, c_float,
  c_double, c_bool, c_boolean
) VALUES (
  ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
);

-- name: ListDerived :many
-- What sqlc types otherwise than a column: a comparison is its `bool`, a
-- cast its type in lowercase.
SELECT
  c_integer IS NOT NULL AS filled,
  CAST(coalesce(c_integer, 0) AS INTEGER) AS as_integer,
  CAST(coalesce(c_integer, 0) AS REAL) AS as_real,
  CAST(coalesce(c_integer, 0) AS TEXT) AS as_text,
  CAST(coalesce(c_integer, 0) AS BLOB) AS as_blob
FROM scalars;
