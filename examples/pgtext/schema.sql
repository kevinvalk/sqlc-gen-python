-- PostgreSQL types psycopg returns as their text, and asyncpg as objects of
-- its own: the psycopg backend maps them to `str`, the SQLAlchemy backend to
-- nothing (use an override).
CREATE TABLE shapes (
  id bigserial PRIMARY KEY,
  flags bit(3) NOT NULL,
  mask varbit NOT NULL,
  spot point NOT NULL,
  edge line NOT NULL,
  segment lseg NOT NULL,
  frame box NOT NULL,
  route path NOT NULL,
  outline polygon NOT NULL,
  area circle NOT NULL,
  lsn pg_lsn NOT NULL
);
