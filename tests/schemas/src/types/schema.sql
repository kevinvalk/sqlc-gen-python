-- A schema exercising the breadth of the PostgreSQL type table, comments,
-- arrays and a non-default schema.

CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');

CREATE TABLE scalars (
    id            bigserial PRIMARY KEY,
    small         smallint NOT NULL,
    regular       integer NOT NULL,
    big           bigint NOT NULL,
    approximate   double precision NOT NULL,
    exact         numeric(10, 2) NOT NULL,
    price         money NOT NULL,
    flag          boolean NOT NULL,
    label         text NOT NULL,
    fixed         char(3) NOT NULL,
    varying       varchar(255) NOT NULL,
    raw           bytea NOT NULL,
    identifier    uuid NOT NULL,
    document      jsonb NOT NULL,
    day           date NOT NULL,
    clock         time NOT NULL,
    moment        timestamptz NOT NULL,
    duration      interval NOT NULL,
    address       inet NOT NULL,
    feeling       mood NOT NULL
);

COMMENT ON TABLE scalars IS 'One column per PostgreSQL type worth mapping.';
COMMENT ON COLUMN scalars.exact IS 'Exact numeric, so this must not become a float.';
COMMENT ON COLUMN scalars.moment IS E'Timezone-aware.\nThe driver returns an aware datetime.';

CREATE TABLE nullables (
    required  text NOT NULL,
    optional  text,
    tags      text[] NOT NULL,
    maybe_tags text[],
    grid      integer[][]
);

CREATE SCHEMA analytics;

CREATE TYPE analytics.event_kind AS ENUM ('click', 'page-view');

CREATE TABLE analytics.events (
    id    bigserial PRIMARY KEY,
    kind  analytics.event_kind NOT NULL
);
