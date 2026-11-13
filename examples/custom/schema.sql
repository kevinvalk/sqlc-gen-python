-- Types the plugin does not map, and names Python should not keep: what
-- overrides and renames are for.
CREATE EXTENSION IF NOT EXISTS ltree;

CREATE DOMAIN positive_amount AS numeric(12, 2) CHECK (VALUE > 0);

CREATE TABLE devices (
  id bigserial PRIMARY KEY,
  ip inet NOT NULL,
  location ltree,
  budget positive_amount NOT NULL
);
