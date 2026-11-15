-- name: ListScalars :many
SELECT * FROM scalars LIMIT $1;

-- name: AddScalars :exec
INSERT INTO scalars (
  c_hstore, c_bit, c_varbit, c_point, c_line, c_lseg, c_box, c_path, c_polygon, c_circle,
  c_cid, c_xid, c_xid8, c_tid, c_pg_lsn,
  c_int4range, c_int8range, c_numrange, c_daterange, c_tsrange, c_tstzrange
) VALUES (
  $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
  $11, $12, $13, $14, $15,
  $16, $17, $18, $19, $20, $21
);
