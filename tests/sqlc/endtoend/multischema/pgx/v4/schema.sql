-- query.sql
CREATE TABLE bar (id serial not null);
CREATE TABLE foo (id serial not null, bar serial);

-- name: ListBar :many
SELECT * FROM bar;
;
-- sql/query.sql
-- name: ListFoo :many
SELECT * FROM foo;
;
