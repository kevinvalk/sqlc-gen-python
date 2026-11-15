-- A column of every affinity the plugin maps on SQLite, by each of SQLite's
-- rules for a declared type, each nullable: the runtime proof
-- (tests/python/typemap.py) stores each edge of a type in its column, alone in
-- its row, and reads it back through every driver.
CREATE TABLE scalars (
  c_integer INTEGER,
  c_bigint BIGINT,
  c_text TEXT,
  c_varchar VARCHAR(20),
  c_clob CLOB,
  c_blob BLOB,
  c_real REAL,
  c_float FLOAT,
  c_double DOUBLE PRECISION,
  c_bool BOOL,
  c_boolean BOOLEAN
);
