-- sql/character.sql
-- Character Types
-- https://www.postgresql.org/docs/current/datatype-character.html
CREATE TABLE dt_character (
    a text,
    b character varying(32),
    c varchar(32),
    d character(32),
    e char(32)
);

CREATE TABLE dt_character_not_null (
    a text NOT NULL,
    b character varying(32) NOT NULL,
    c varchar(32) NOT NULL,
    d character(32) NOT NULL,
    e char(32) NOT NULL
);
;
-- sql/datetime.sql
-- Date/Time Types
-- https://www.postgresql.org/docs/current/datatype-datetime.html
CREATE TABLE dt_datetime (
    a DATE,
    b TIME,
    c TIME WITHOUT TIME ZONE,
    d TIME WITH TIME ZONE,
    e TIMESTAMP,
    f TIMESTAMP WITHOUT TIME ZONE,
    g TIMESTAMP WITH TIME ZONE,
    h timestamptz
);

CREATE TABLE dt_datetime_not_null (
    a DATE NOT NULL,
    b TIME NOT NULL,
    c TIME WITHOUT TIME ZONE NOT NULL,
    d TIME WITH TIME ZONE NOT NULL,
    e TIMESTAMP NOT NULL,
    f TIMESTAMP WITHOUT TIME ZONE NOT NULL,
    g TIMESTAMP WITH TIME ZONE NOT NULL,
    h timestamptz NOT NULL
);
;
-- sql/net-types.sql
-- Network Address Types
-- https://www.postgresql.org/docs/current/datatype-net-types.html
CREATE TABLE dt_net_types (
  a  inet,
  b  cidr,
  c  macaddr
);

CREATE TABLE dt_net_types_not_null (
  a  inet NOT NULL,
  b  cidr NOT NULL,
  c  macaddr NOT NULL
);
;
-- sql/numeric.sql
-- Numeric Types
-- https://www.postgresql.org/docs/current/datatype-numeric.html
CREATE TABLE dt_numeric (
    -- TODO: this maps incorrectly to int16, not NullInt16
    a smallint,
    b integer,
    c bigint,
    d decimal,
    e numeric,
    f real,
    g double precision,
    -- TODO: this maps incorrectly to int16, not NullInt16
    h smallserial,
    i serial,
    j bigserial,
    -- TODO: this maps incorrectly to int16, not NullInt16
    k int2,
    l int4,
    m int8
);

CREATE TABLE dt_numeric_not_null (
    a smallint NOT NULL,
    b integer NOT NULL,
    c bigint NOT NULL,
    d decimal NOT NULL,
    e numeric NOT NULL,
    f real NOT NULL,
    g double precision NOT NULL,
    h smallserial NOT NULL,
    i serial NOT NULL,
    j bigserial NOT NULL,
    k int2 NOT NULL,
    l int4 NOT NULL,
    m int8 NOT NULL
);
;
-- sql/query.sql
-- name: Test :one
SELECT 1;
;
-- sql/rangetypes.sql
-- Range Types
-- https://www.postgresql.org/docs/current/rangetypes.html
CREATE TABLE dt_range (
    a int4range,
    b int8range,
    c numrange,
    d tsrange,
    e tstzrange,
    f daterange
);

CREATE TABLE dt_range_not_null (
    a int4range NOT NULL,
    b int8range NOT NULL,
    c numrange NOT NULL,
    d tsrange NOT NULL,
    e tstzrange NOT NULL,
    f daterange NOT NULL
);
;
