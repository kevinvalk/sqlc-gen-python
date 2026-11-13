-- sql/character.sql
-- Character Types
-- https://www.sqlite.org/datatype3.html
CREATE TABLE dt_character (
    a CHARACTER(32),
    b VARCHAR(32),
    c VARYING CHARACTER(32),
    d NCHAR(32),
    e NATIVE CHARACTER(32),
    f NVARCHAR(32),
    g TEXT,
    h CLOB
);

CREATE TABLE dt_character_not_null (
    a CHARACTER(32) NOT NULL,
    b VARCHAR(32) NOT NULL,
    c VARYING CHARACTER(32) NOT NULL,
    d NCHAR(32) NOT NULL,
    e NATIVE CHARACTER(32) NOT NULL,
    f NVARCHAR(32) NOT NULL,
    g TEXT NOT NULL,
    h CLOB NOT NULL
);
;
-- sql/datetime.sql
-- Date/Time Types
-- https://www.sqlite.org/datatype3.html
CREATE TABLE dt_datetime (
    a DATE,
    b DATETIME,
    c TIMESTAMP
);

CREATE TABLE dt_datetime_not_null (
    a DATE NOT NULL,
    b DATETIME NOT NULL,
    c TIMESTAMP NOT NULL
);
;
-- sql/numeric.sql
-- Numeric Types
-- https://www.sqlite.org/datatype3.html
CREATE TABLE dt_numeric (
    a INT,
    b INTEGER,
    c TINYINT,
    d SMALLINT,
    e MEDIUMINT,
    f BIGINT,
    g UNSIGNED BIG INT,
    h INT2,
    i INT8,
    j REAL,
    k DOUBLE,
    l DOUBLE PRECISION,
    m FLOAT,
    n NUMERIC,
    o DECIMAL(10,5)
);

CREATE TABLE dt_numeric_not_null (
    a INT NOT NULL,
    b INTEGER NOT NULL,
    c TINYINT NOT NULL,
    d SMALLINT NOT NULL,
    e MEDIUMINT NOT NULL,
    f BIGINT NOT NULL,
    g UNSIGNED BIG INT NOT NULL,
    h INT2 NOT NULL,
    i INT8 NOT NULL,
    j REAL NOT NULL,
    k DOUBLE NOT NULL,
    l DOUBLE PRECISION NOT NULL,
    m FLOAT NOT NULL,
    n NUMERIC NOT NULL,
    o DECIMAL(10,5) NOT NULL
);
;
-- sql/query.sql
-- name: Test :one
SELECT 1;
;
