-- Stage a static target without changing the active interface address or its
-- cross-lifetime MAC reservation. Global UNIQUE(address) protects all allocators.
ALTER TABLE machine_interface_addresses
    ADD COLUMN staged_for_interface_id uuid REFERENCES machine_interfaces(id) ON DELETE RESTRICT,
    DROP CONSTRAINT machine_interface_addresses_owner_check,
    ADD CONSTRAINT machine_interface_addresses_owner_check CHECK (
        interface_id IS NOT NULL OR reserved_by_mac IS NOT NULL OR staged_for_interface_id IS NOT NULL
    ),
    ADD CONSTRAINT machine_interface_addresses_staged_check CHECK (
        staged_for_interface_id IS NULL OR (interface_id IS NULL AND reserved_by_mac IS NULL)
    );
CREATE UNIQUE INDEX machine_interface_addresses_staged_family_key
    ON machine_interface_addresses (staged_for_interface_id, family(address))
    WHERE staged_for_interface_id IS NOT NULL;
