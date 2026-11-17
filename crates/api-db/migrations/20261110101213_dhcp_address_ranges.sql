-- Nullable bounds retain the revision after clearing a range.
CREATE TABLE network_dhcp_ranges (
    prefix_id UUID PRIMARY KEY REFERENCES network_prefixes(id) ON DELETE CASCADE,
    range_start INET,
    range_end INET,
    revision BIGINT NOT NULL CHECK (revision > 0),
    CHECK (
        (range_start IS NULL AND range_end IS NULL)
        OR (range_start IS NOT NULL AND range_end IS NOT NULL
            AND family(range_start) = 4 AND family(range_end) = 4
            AND masklen(range_start) = 32 AND masklen(range_end) = 32
            AND range_start <= range_end)
    )
);
