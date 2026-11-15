-- name: ListScalars :many
SELECT * FROM scalars LIMIT ?;

-- name: AddScalars :exec
INSERT INTO scalars (
  c_bool, c_tinyint, c_tinyint_unsigned, c_smallint, c_smallint_unsigned,
  c_mediumint, c_mediumint_unsigned, c_int, c_int_unsigned, c_bigint,
  c_bigint_unsigned, c_year, c_float, c_double, c_decimal, c_char, c_varchar,
  c_tinytext, c_text, c_mediumtext, c_longtext, c_json, c_binary,
  c_varbinary, c_tinyblob, c_blob, c_mediumblob, c_longblob, c_bit_1,
  c_bit_64, c_date, c_datetime, c_timestamp, c_time
) VALUES (
  ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
  ?, ?, ?, ?, ?, ?, ?, ?, ?
);

-- name: ListDerived :many
-- What sqlc types otherwise than a column: a comparison is its `bool`, a
-- cast to an integer its `bigint signed`.
SELECT
  c_bigint IS NOT NULL AS filled,
  CAST(coalesce(c_bigint, 0) AS SIGNED) AS as_signed
FROM scalars;
