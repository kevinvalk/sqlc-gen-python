-- Types the plugin does not map, and names Python should not keep: what
-- overrides and renames are for.
CREATE EXTENSION IF NOT EXISTS ltree;

CREATE DOMAIN positive_amount AS numeric(12, 2) CHECK (VALUE > 0);

-- How a device signs in. ruff's S105 takes a name such as these labels',
-- or a query's that ends in one, for a secret's: a module that assigns one
-- says it holds none.
CREATE TYPE sign_in AS ENUM ('password', 'token');

CREATE TABLE devices (
  id bigserial PRIMARY KEY,
  -- An IPv4 address, as the override that types it says: a host's, as with
  -- a netmask the drivers return an interface.
  ip inet NOT NULL CHECK (family(ip) = 4 AND masklen(ip) = 32),
  location ltree,
  budget positive_amount NOT NULL,
  sign_in sign_in NOT NULL DEFAULT 'token'
);
