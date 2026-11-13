-- sql/character.sql
-- Character Types
-- https://dev.mysql.com/doc/refman/8.0/en/string-type-syntax.html
CREATE TABLE dt_character (
    a CHARACTER(32),
    b VARCHAR(32),
    c CHAR(32),
    d BINARY(32),
    e VARBINARY(32),
    f TINYBLOB,
    g TINYTEXT,
    h TEXT,
    i MEDIUMTEXT,
    j MEDIUMBLOB,
    k LONGTEXT,
    l LONGBLOB
);

CREATE TABLE dt_character_not_null (
    a CHARACTER(32) NOT NULL,
    b VARCHAR(32) NOT NULL,
    c CHAR(32) NOT NULL,
    d BINARY(32) NOT NULL,
    e VARBINARY(32) NOT NULL,
    f TINYBLOB NOT NULL,
    g TINYTEXT NOT NULL,
    h TEXT NOT NULL,
    i MEDIUMTEXT NOT NULL,
    j MEDIUMBLOB NOT NULL,
    k LONGTEXT NOT NULL,
    l LONGBLOB NOT NULL
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
-- https://dev.mysql.com/doc/refman/8.0/en/numeric-type-syntax.html
CREATE TABLE dt_numeric (
    a INT,
    b INTEGER,
    c TINYINT,
    d SMALLINT,
    e MEDIUMINT,
    f BIGINT,
    g BIT,
    h DECIMAL(10, 5),
    i DEC(10, 5),
    j FLOAT,
    k DOUBLE,
    l DOUBLE PRECISION
);

CREATE TABLE dt_numeric_unsigned (
    a INT UNSIGNED,
    b INTEGER UNSIGNED,
    c TINYINT UNSIGNED,
    d SMALLINT UNSIGNED,
    e MEDIUMINT UNSIGNED,
    f BIGINT UNSIGNED
);

CREATE TABLE dt_numeric_not_null (
    a INT NOT NULL,
    b INTEGER NOT NULL,
    c TINYINT NOT NULL,
    d SMALLINT NOT NULL,
    e MEDIUMINT NOT NULL,
    f BIGINT NOT NULL,
    g BIT NOT NULL,
    h DECIMAL(10, 5) NOT NULL,
    i DEC(10, 5) NOT NULL,
    j FLOAT NOT NULL,
    k DOUBLE NOT NULL,
    l DOUBLE PRECISION NOT NULL
);
;
-- sql/query.sql
-- name: Test :one
SELECT 1;
;
