-- A schema exercising the breadth of the PostgreSQL type table, comments,
-- arrays and a non-default schema. Every type the plugin maps has a column in
-- `scalars` and, unless its arrays are an error, one in `arrays`, so the
-- runtime proof (tests/py/typemap.py) reads back each of them.

CREATE EXTENSION IF NOT EXISTS citext;
CREATE EXTENSION IF NOT EXISTS hstore;
CREATE EXTENSION IF NOT EXISTS ltree;

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
    feeling       mood NOT NULL,
    object        oid NOT NULL,
    single        real NOT NULL,
    title         name NOT NULL,
    email         citext NOT NULL,
    mac           macaddr NOT NULL,
    mac8          macaddr8 NOT NULL,
    markup        xml NOT NULL,
    search        tsvector NOT NULL,
    wall          timetz NOT NULL,
    naive         timestamp NOT NULL,
    plain_json    json NOT NULL,
    network       cidr NOT NULL,
    pairs         hstore NOT NULL,
    tree          ltree NOT NULL,
    relation      regclass NOT NULL,
    span          int4range NOT NULL,
    big_span      int8range NOT NULL,
    amounts       numrange NOT NULL,
    days          daterange NOT NULL,
    times         tsrange NOT NULL,
    moments       tstzrange NOT NULL
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

-- One array column per mapped type but `citext` and `tsvector`, whose arrays
-- are an error.
CREATE TABLE arrays (
    int2s         int2[] NOT NULL,
    int4s         int4[] NOT NULL,
    int8s         int8[] NOT NULL,
    oids          oid[] NOT NULL,
    float4s       float4[] NOT NULL,
    float8s       float8[] NOT NULL,
    numerics      numeric[] NOT NULL,
    moneys        money[] NOT NULL,
    bools         bool[] NOT NULL,
    texts         text[] NOT NULL,
    varchars      varchar[] NOT NULL,
    bpchars       bpchar[] NOT NULL,
    names         name[] NOT NULL,
    macaddrs      macaddr[] NOT NULL,
    macaddr8s     macaddr8[] NOT NULL,
    xmls          xml[] NOT NULL,
    byteas        bytea[] NOT NULL,
    dates         date[] NOT NULL,
    times         time[] NOT NULL,
    timetzs       timetz[] NOT NULL,
    timestamps    timestamp[] NOT NULL,
    timestamptzs  timestamptz[] NOT NULL,
    intervals     interval[] NOT NULL,
    uuids         uuid[] NOT NULL,
    jsons         json[] NOT NULL,
    jsonbs        jsonb[] NOT NULL,
    inets         inet[] NOT NULL,
    cidrs         cidr[] NOT NULL
);

CREATE SCHEMA analytics;

CREATE TYPE analytics.event_kind AS ENUM ('click', 'page-view');

CREATE TABLE analytics.events (
    id    bigserial PRIMARY KEY,
    kind  analytics.event_kind NOT NULL
);

-- A type whose name needs quoting, which the runtime proof must look up as
-- sqlc spells it.
CREATE TYPE "weird mood" AS ENUM ('meh', 'fine');

CREATE TABLE oddities (
    id    bigserial PRIMARY KEY,
    mood  "weird mood" NOT NULL
);
