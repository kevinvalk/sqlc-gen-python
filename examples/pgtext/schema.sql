-- Every PostgreSQL type the psycopg backend alone maps, in a nullable column
-- each. psycopg returns the first fifteen as their text, where asyncpg
-- returns objects of its own, and a range as its own `Range`: the
-- SQLAlchemy backend maps the ranges through its own types
-- (`examples/pgtypes`), and the others to nothing (use an override).
CREATE EXTENSION IF NOT EXISTS hstore;

CREATE TABLE scalars (
  c_hstore hstore,
  c_bit bit(3),
  c_varbit varbit,
  c_point point,
  c_line line,
  c_lseg lseg,
  c_box box,
  c_path path,
  c_polygon polygon,
  c_circle circle,
  c_cid cid,
  c_xid xid,
  c_xid8 xid8,
  c_tid tid,
  c_pg_lsn pg_lsn,
  c_int4range int4range,
  c_int8range int8range,
  c_numrange numrange,
  c_daterange daterange,
  c_tsrange tsrange,
  c_tstzrange tstzrange
);
